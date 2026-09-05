<script lang="ts">
import { ref } from "vue";
// 全局共享单例：保证整个应用同一时刻仅展开一个下拉框
const globalActiveSelectId = ref<string | null>(null);
</script>

<script setup lang="ts">
import { computed, nextTick, onUnmounted, ref as vueRef, useAttrs, watch } from "vue";
import { icons } from "../../icons";

const attrs = useAttrs();
/**
 * 菜单已 Teleport 到 body，不再位于组件根内部：把根上挂的自定义 class
 * （去掉 select-box 本身）同步到菜单，保证 `.xxx .select-menu` 形式的
 * 页面级样式覆盖继续生效。
 */
const menuClass = computed(() =>
  String(attrs.class ?? "")
    .split(/\s+/)
    .filter((cls) => cls && cls !== "select-box" && cls !== "open" && cls !== "flip-up"),
);

interface Option {
  value: string | number;
  text: string;
}

/** 分组下拉：每组一个标题 + 一组选项；与扁平 options 共存时扁平项渲染在最前。 */
interface OptionGroup {
  label: string;
  options: Option[];
}

const props = withDefaults(
  defineProps<{
    options: Option[];
    modelValue: string | number;
    ariaLabel?: string;
    placement?: "auto" | "top" | "bottom";
    /** 菜单最小宽度（px）。行内紧凑下拉可设小（如 120），避免级联列表被截断。 */
    menuMinWidth?: number;
    /** 按组渲染（如按国家/分组名）；缺省为纯扁平列表。 */
    groups?: OptionGroup[];
    /**
     * 让 trigger 自动撑到「最长选项」的宽度，而非跟随当前选中项。
     * 用于行内紧凑下拉：固定 px 宽度容不下长选项会被省略号截断，
     * 而跟随选中项又会让盒子随选择跳动。开启后宽度恒定且不截断。
     * 需要上限时在调用方用 max-width 约束。
     */
    autoWidth?: boolean;
    /** 是否启用搜索功能 */
    searchable?: boolean;
    /** 搜索框占位文本 */
    searchPlaceholder?: string;
  }>(),
  {
    ariaLabel: undefined,
    placement: "auto",
    menuMinWidth: undefined,
    groups: undefined,
    autoWidth: false,
    searchable: false,
    searchPlaceholder: "搜索...",
  }
);

const emit = defineEmits<{
  "update:modelValue": [value: any];
}>();

const instanceId = `select-${Math.random().toString(36).substring(2, 9)}`;
const rootRef = vueRef<HTMLElement>();
const triggerRef = vueRef<HTMLButtonElement>();
const menuRef = vueRef<HTMLElement>();
const searchInputRef = vueRef<HTMLInputElement>();
const selectedText = vueRef("");
const searchQuery = vueRef("");

const isOpen = computed(() => globalActiveSelectId.value === instanceId);
const flipUp = vueRef(false);

// 搜索过滤逻辑
const filteredOptions = computed(() => {
  if (!props.searchable || !searchQuery.value.trim()) {
    return props.options;
  }
  const query = searchQuery.value.toLowerCase();
  return props.options.filter(opt =>
    opt.text.toLowerCase().includes(query) ||
    String(opt.value).toLowerCase().includes(query)
  );
});

const filteredGroups = computed(() => {
  if (!props.searchable || !searchQuery.value.trim() || !props.groups) {
    return props.groups;
  }
  const query = searchQuery.value.toLowerCase();
  return props.groups
    .map(group => ({
      ...group,
      options: group.options.filter(opt =>
        opt.text.toLowerCase().includes(query) ||
        String(opt.value).toLowerCase().includes(query)
      )
    }))
    .filter(group => group.options.length > 0);
});

// 菜单采用 fixed 定位（相对视口），不依赖 overflow 容器：弹窗/滚动容器
// 内展开时不会再被 overflow 裁切。坐标在 open 时按 trigger 实时计算。
const menuStyle = ref<Record<string, string>>({});

function computeMenuPosition() {
  const trigger = triggerRef.value;
  const menu = menuRef.value;
  if (!trigger) return;
  const rect = trigger.getBoundingClientRect();
  const menuWidth = Math.max(rect.width, props.menuMinWidth ?? 0, 190);
  const menuHeight = menu && menu.offsetHeight > 0 ? menu.offsetHeight : Math.max(120, (props.options.length || 3) * 34 + 10);
  const spaceBelow = window.innerHeight - rect.bottom;
  const spaceAbove = rect.top;

  if (props.placement === "top") {
    flipUp.value = true;
  } else if (props.placement === "bottom") {
    flipUp.value = false;
  } else {
    flipUp.value = (spaceBelow < menuHeight + 12 && spaceAbove > spaceBelow) || spaceBelow < 120;
  }

  const left = Math.max(8, Math.min(rect.left, window.innerWidth - menuWidth - 8));
  menuStyle.value = flipUp.value
    ? {
        position: "fixed",
        left: `${left}px`,
        bottom: `${window.innerHeight - rect.top + 6}px`,
        width: `${menuWidth}px`,
      }
    : {
        position: "fixed",
        left: `${left}px`,
        top: `${rect.bottom + 6}px`,
        width: `${menuWidth}px`,
      };
}

function syncDisplay() {
  const selected =
    props.options.find((opt) => String(opt.value) === String(props.modelValue)) ??
    props.groups?.flatMap((group) => group.options).find((opt) => String(opt.value) === String(props.modelValue));
  selectedText.value = selected?.text ?? "";
}

/**
 * autoWidth 模式的隐形撑宽文案：取所有选项中视觉最宽的一条。
 * CJK 字符约为西文的两倍宽，按此估算挑选候选，最终宽度仍由浏览器实际排版决定。
 */
/** 全角字符区间：CJK 标点、假名、汉字、全角字形 —— 计宽按西文字符的两倍 */
const FULL_WIDTH_CHAR = /[\u3000-\u303f\u3040-\u30ff\u4e00-\u9fff\uff00-\uffef]/;

const widestOptionText = computed(() => {
  if (!props.autoWidth) return "";
  const all = [...props.options, ...(props.groups?.flatMap((g) => g.options) ?? [])];
  let widest = "";
  let widestScore = -1;
  for (const opt of all) {
    const score = [...opt.text].reduce(
      (sum, ch) => sum + (FULL_WIDTH_CHAR.test(ch) ? 2 : 1),
      0,
    );
    if (score > widestScore) {
      widestScore = score;
      widest = opt.text;
    }
  }
  return widest;
});

watch(() => [props.options, props.modelValue], syncDisplay, { deep: true, immediate: true });

function toggle(e?: Event) {
  e?.stopPropagation();
  if (isOpen.value) {
    close();
  } else {
    open();
  }
}

function open() {
  globalActiveSelectId.value = instanceId;
  searchQuery.value = "";
  nextTick(() => {
    computeMenuPosition();
    requestAnimationFrame(computeMenuPosition);
    // 如果启用了搜索，自动聚焦搜索框
    if (props.searchable) {
      nextTick(() => searchInputRef.value?.focus());
    }
  });
}

function close() {
  if (globalActiveSelectId.value === instanceId) {
    globalActiveSelectId.value = null;
  }
}

function selectOption(value: string | number, e?: Event) {
  e?.stopPropagation();
  emit("update:modelValue", value);
  close();
  triggerRef.value?.focus();
}

function onKeydown(event: KeyboardEvent) {
  if (event.key === "Escape") {
    close();
    triggerRef.value?.focus();
    return;
  }
  if (event.key === "Enter" || event.key === " ") {
    if (!isOpen.value) {
      event.preventDefault();
      open();
    }
    return;
  }
  if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
  event.preventDefault();
  if (!isOpen.value) open();
  const optionEls = menuRef.value
    ? [...menuRef.value.querySelectorAll<HTMLButtonElement>(".select-option")]
    : [];
  if (!optionEls.length) return;
  const current = optionEls.indexOf(document.activeElement as HTMLButtonElement);
  const next =
    event.key === "ArrowDown"
      ? Math.min(current + 1, optionEls.length - 1)
      : Math.max(current < 0 ? optionEls.length - 1 : current - 1, 0);
  optionEls[next]?.focus();
}

function onDocumentClick(event: Event) {
  if (!isOpen.value) return;
  const target = event.target as Node | null;
  // 菜单已 Teleport 到组件外：菜单内的点击（含选项按下）不能视为外部点击
  if (rootRef.value && target && rootRef.value.contains(target)) return;
  if (menuRef.value && target && menuRef.value.contains(target)) return;
  close();
}

watch(isOpen, (openState) => {
  if (openState) {
    window.addEventListener("pointerdown", onDocumentClick, { capture: true });
    window.addEventListener("click", onDocumentClick, { capture: true });
  } else {
    window.removeEventListener("pointerdown", onDocumentClick, { capture: true });
    window.removeEventListener("click", onDocumentClick, { capture: true });
  }
});

// 打开期间滚动/缩放跟随重算，避免菜单悬浮在错位的位置
function onViewportChange() {
  if (isOpen.value) computeMenuPosition();
}
window.addEventListener("resize", onViewportChange);
window.addEventListener("scroll", onViewportChange, true);

onUnmounted(() => {
  window.removeEventListener("resize", onViewportChange);
  window.removeEventListener("scroll", onViewportChange, true);
  window.removeEventListener("pointerdown", onDocumentClick, { capture: true });
  window.removeEventListener("click", onDocumentClick, { capture: true });
  if (globalActiveSelectId.value === instanceId) {
    globalActiveSelectId.value = null;
  }
});
</script>

<template>
  <div
    ref="rootRef"
    class="select-box"
    :class="{ open: isOpen, 'flip-up': flipUp, 'auto-width': autoWidth }"
    data-custom-select
    @keydown="onKeydown"
  >
    <select
      :value="modelValue"
      tabindex="-1"
      aria-hidden="true"
      :aria-label="ariaLabel"
      @change="$emit('update:modelValue', ($event.target as HTMLSelectElement).value)"
    >
      <option v-for="opt in options" :key="String(opt.value)" :value="opt.value">
        {{ opt.text }}
      </option>
      <optgroup v-for="group in groups ?? []" :key="group.label" :label="group.label">
        <option v-for="opt in group.options" :key="String(opt.value)" :value="opt.value">
          {{ opt.text }}
        </option>
      </optgroup>
    </select>
    <button
      ref="triggerRef"
      class="select-trigger"
      type="button"
      aria-haspopup="listbox"
      :aria-expanded="isOpen"
      @click="toggle"
    >
      <!-- autoWidth：以最长选项占位撑宽（aria-hidden，仅参与排版），
           可见文案绝对定位覆盖其上，使盒宽恒等于最长选项、且不随选中项跳动 -->
      <span v-if="autoWidth" class="select-label-auto">
        <span class="select-label-sizer" aria-hidden="true">{{ widestOptionText }}</span>
        <span class="select-label-shown">{{ selectedText }}</span>
      </span>
      <span v-else>{{ selectedText }}</span>
      <span v-html="icons.chevron" />
    </button>
    <Teleport to="body">
      <div
        v-if="isOpen"
        ref="menuRef"
        class="select-menu"
        :class="[menuClass, { 'flip-up': flipUp }]"
        :style="menuStyle"
        role="listbox"
      >
        <div v-if="searchable" class="select-search-box">
          <input
            ref="searchInputRef"
            v-model="searchQuery"
            type="text"
            class="select-search-input"
            :placeholder="searchPlaceholder"
            @keydown.stop
            @click.stop
          />
        </div>
        <div v-if="$slots['menu-header']" class="select-menu-header">
          <slot name="menu-header" />
        </div>
        <div class="select-options-list">
          <button
            v-for="opt in filteredOptions"
            :key="String(opt.value)"
            class="select-option"
            type="button"
            role="option"
            :class="{ selected: String(opt.value) === String(modelValue) }"
            :aria-selected="String(opt.value) === String(modelValue)"
            :data-select-value="opt.value"
            @click="selectOption(opt.value, $event)"
          >
            {{ opt.text }}
          </button>
          <template v-for="group in filteredGroups ?? []" :key="group.label">
            <div class="select-group-label" role="presentation">{{ group.label }}</div>
            <button
              v-for="opt in group.options"
              :key="String(opt.value)"
              class="select-option"
              type="button"
              role="option"
              :class="{ selected: String(opt.value) === String(modelValue) }"
              :aria-selected="String(opt.value) === String(modelValue)"
              :data-select-value="opt.value"
              @click="selectOption(opt.value, $event)"
            >
              {{ opt.text }}
            </button>
          </template>
          <div
            v-if="filteredOptions.length === 0 && (!filteredGroups || filteredGroups.length === 0)"
            class="select-no-results"
          >
            无匹配结果
          </div>
        </div>
      </div>
    </Teleport>
  </div>
</template>
