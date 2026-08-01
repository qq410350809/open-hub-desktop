import { ref } from "vue";

// —— 全局单例 ——
const message = ref("");
const isError = ref(false);
const visible = ref(false);

let toastTimer: number | undefined;

export function useToast() {
  function showToast(msg: string, error = false) {
    message.value = msg;
    isError.value = error;
    visible.value = true;
    window.clearTimeout(toastTimer);
    toastTimer = window.setTimeout(() => {
      visible.value = false;
    }, 2300);
  }

  return { message, isError, visible, showToast };
}
