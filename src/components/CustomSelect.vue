<script lang="ts">
import { ref } from "vue";
// 全局共享单例：保证整个应用同一时刻仅展开一个下拉框
const globalActiveSelectId = ref<string | null>(null);
</script>

<script setup lang="ts">
import { computed, watch, onUnmounted } from "vue";
import { icons } from "../icons";

interface Option {
  value: string | number;
  text: string;
}

const props = withDefaults(
  defineProps<{
    options: Option[];
    modelValue: string | number;
    ariaLabel?: string;
    placement?: "auto" | "top" | "bottom";
  }>(),
  {
    ariaLabel: undefined,
    placement: "auto",
  }
);

const emit = defineEmits<{
  "update:modelValue": [value: any];
}>();

const instanceId = `select-${Math.random().toString(36).substring(2, 9)}`;
const rootRef = ref<HTMLElement>();
const triggerRef = ref<HTMLButtonElement>();
const menuRef = ref<HTMLElement>();
const selectedText = ref("");

const isOpen = computed(() => globalActiveSelectId.value === instanceId);
const flipUp = ref(false);

// 下拉菜单默认向下展开；当位于底部或明确配置 placement="top" 时向上展开，避免被遮挡
function getScrollParent(el: HTMLElement): HTMLElement | null {
  let node = el.parentElement;
  while (node) {
    const overflowY = getComputedStyle(node).overflowY;
    if (overflowY === "auto" || overflowY === "scroll" || overflowY === "overlay") return node;
    node = node.parentElement;
  }
  return null;
}

function measurePlacement() {
  if (props.placement === "top") {
    flipUp.value = true;
    return;
  }
  if (props.placement === "bottom") {
    flipUp.value = false;
    return;
  }

  const trigger = triggerRef.value;
  const root = rootRef.value;
  if (!trigger || !root) return;

  const menu = menuRef.value;
  const menuHeight = menu && menu.offsetHeight > 0 ? menu.offsetHeight : Math.max(120, (props.options.length || 3) * 32 + 12);
  const triggerRect = trigger.getBoundingClientRect();
  const scrollParent = getScrollParent(root);
  let spaceBelow: number;
  let spaceAbove: number;

  if (scrollParent) {
    const parentRect = scrollParent.getBoundingClientRect();
    spaceBelow = parentRect.bottom - triggerRect.bottom;
    spaceAbove = triggerRect.top - parentRect.top;
  } else {
    spaceBelow = window.innerHeight - triggerRect.bottom;
    spaceAbove = triggerRect.top;
  }

  // 当下方空间不足以容纳菜单且上方空间更多，或者下方空间小于 140px 时，自动翻转向上
  flipUp.value = (spaceBelow < menuHeight && spaceAbove > spaceBelow) || spaceBelow < 120;
}

function syncDisplay() {
  const selected = props.options.find((opt) => String(opt.value) === String(props.modelValue));
  selectedText.value = selected?.text ?? "";
}

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
  measurePlacement();
  requestAnimationFrame(measurePlacement);
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
  if (rootRef.value && target && !rootRef.value.contains(target)) {
    close();
  }
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

onUnmounted(() => {
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
    :class="{ open: isOpen, 'flip-up': flipUp }"
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
    </select>
    <button
      ref="triggerRef"
      class="select-trigger"
      type="button"
      aria-haspopup="listbox"
      :aria-expanded="isOpen"
      @click="toggle"
    >
      <span>{{ selectedText }}</span>
      <span v-html="icons.chevron" />
    </button>
    <div ref="menuRef" class="select-menu" role="listbox" :hidden="!isOpen">
      <button
        v-for="opt in options"
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
    </div>
  </div>
</template>
