<script setup lang="ts">
import { ref, computed, watch, onUnmounted } from "vue";
import { icons } from "../icons";

interface Option {
  value: string | number;
  text: string;
}

const props = defineProps<{
  options: Option[];
  modelValue: string | number;
  ariaLabel?: string;
}>();

const emit = defineEmits<{
  "update:modelValue": [value: any];
}>();

// Shared active select manager ref so only one dropdown is active at a time across the app
const activeSelectId = ref<string | null>(null);

const instanceId = `select-${Math.random().toString(36).substring(2, 9)}`;
const rootRef = ref<HTMLElement>();
const triggerRef = ref<HTMLButtonElement>();
const menuRef = ref<HTMLElement>();
const selectedText = ref("");

const isOpen = computed(() => activeSelectId.value === instanceId);

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
  activeSelectId.value = instanceId;
}

function close() {
  if (activeSelectId.value === instanceId) {
    activeSelectId.value = null;
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
  if (activeSelectId.value === instanceId) {
    activeSelectId.value = null;
  }
});
</script>

<template>
  <div
    ref="rootRef"
    class="select-box"
    :class="{ open: isOpen }"
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
