<script setup lang="ts">
import { onMounted, onUnmounted } from "vue";
import { useConfirm } from "../composables/useConfirm";

const { visible, title, message, confirmText, cancelText, danger, accept, cancel } = useConfirm();

function onKeydown(event: KeyboardEvent) {
  if (!visible.value) return;
  if (event.key === "Escape") {
    event.preventDefault();
    cancel();
  } else if (event.key === "Enter") {
    event.preventDefault();
    accept();
  }
}

onMounted(() => document.addEventListener("keydown", onKeydown, true));
onUnmounted(() => document.removeEventListener("keydown", onKeydown, true));
</script>

<template>
  <Teleport to="body">
    <div v-if="visible" class="confirm-dialog-backdrop" @click.self="cancel">
      <section
        class="confirm-dialog"
        role="alertdialog"
        aria-modal="true"
        aria-labelledby="confirm-dialog-title"
        aria-describedby="confirm-dialog-message"
      >
        <header class="confirm-dialog-header">
          <h2 id="confirm-dialog-title">{{ title }}</h2>
        </header>
        <p id="confirm-dialog-message" class="confirm-dialog-message">{{ message }}</p>
        <footer class="confirm-dialog-footer">
          <button type="button" class="secondary-button" @click="cancel">{{ cancelText }}</button>
          <button
            type="button"
            class="primary-button"
            :class="{ danger: danger }"
            autofocus
            @click="accept"
          >{{ confirmText }}</button>
        </footer>
      </section>
    </div>
  </Teleport>
</template>
