<script setup lang="ts">
import { onBeforeUnmount, onMounted, onUnmounted, ref } from "vue";
import { icons } from "../../icons";

withDefaults(defineProps<{ title: string; drawer?: boolean }>(), { drawer: false });
const emit = defineEmits<{ close: [] }>();
const element = ref<HTMLDialogElement>();
let previousFocus: HTMLElement | null = null;

onMounted(() => {
  previousFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  element.value?.showModal();
});
onBeforeUnmount(() => element.value?.close());
onUnmounted(() => {
  if (previousFocus?.isConnected) previousFocus.focus();
});

function backdrop(event: MouseEvent) {
  if (event.target !== element.value) return;
  const rect = element.value!.getBoundingClientRect();
  if (event.clientX < rect.left || event.clientX > rect.right ||
      event.clientY < rect.top || event.clientY > rect.bottom) {
    emit("close");
  }
}
</script>

<template>
  <Teleport to="body">
    <dialog
      ref="element"
      class="catalog-dialog"
      :class="{ 'is-drawer': drawer }"
      :aria-label="title"
      @keydown.stop
      @cancel.prevent="emit('close')"
      @click="backdrop"
    >
      <div class="catalog-dialog-tools">
        <button
          type="button"
          class="catalog-icon catalog-dialog-close"
          :aria-label="`关闭${title}`"
          autofocus
          @click="emit('close')"
        >
          <span v-html="icons.close" />
        </button>
      </div>
      <slot />
    </dialog>
  </Teleport>
</template>
