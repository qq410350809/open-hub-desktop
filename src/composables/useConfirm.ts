import { ref } from "vue";

// —— 全局单例确认框：替代 window.confirm（Tauri 桌面端 WKWebView 不弹原生 confirm）——

export interface ConfirmOptions {
  title?: string;
  message: string;
  confirmText?: string;
  cancelText?: string;
  danger?: boolean;
}

const visible = ref(false);
const title = ref("");
const message = ref("");
const confirmText = ref("确定");
const cancelText = ref("取消");
const danger = ref(false);

let resolver: ((value: boolean) => void) | null = null;

export function useConfirm() {
  function confirm(options: ConfirmOptions | string): Promise<boolean> {
    // 上一个未决的确认框直接按取消关闭，避免多个确认框互相覆盖。
    if (resolver) {
      const stale = resolver;
      resolver = null;
      stale(false);
    }
    const opts = typeof options === "string" ? { message: options } : options;
    title.value = opts.title ?? "确认操作";
    message.value = opts.message;
    confirmText.value = opts.confirmText ?? "确定";
    cancelText.value = opts.cancelText ?? "取消";
    danger.value = opts.danger ?? false;
    visible.value = true;
    return new Promise<boolean>((resolve) => {
      resolver = resolve;
    });
  }

  function accept() {
    if (!resolver) return;
    const resolve = resolver;
    resolver = null;
    visible.value = false;
    resolve(true);
  }

  function cancel() {
    if (!resolver) return;
    const resolve = resolver;
    resolver = null;
    visible.value = false;
    resolve(false);
  }

  return { visible, title, message, confirmText, cancelText, danger, confirm, accept, cancel };
}
