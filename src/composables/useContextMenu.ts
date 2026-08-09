import { onMounted, onUnmounted, ref } from "vue";

export type ContextMenuItem = {
  id: string;
  label: string;
  enabled: boolean;
  accelerator?: string;
  danger?: boolean;
  separator?: boolean;
};

type EditableTarget =
  | HTMLInputElement
  | HTMLTextAreaElement
  | (HTMLElement & { isContentEditable: true });

const MENU_WIDTH = 196;
const MENU_PAD = 8;

function isEditable(el: EventTarget | null): el is EditableTarget {
  if (!(el instanceof HTMLElement)) return false;
  if (el.isContentEditable) return true;
  if (el instanceof HTMLInputElement) {
    const type = (el.type || "text").toLowerCase();
    return ![
      "button",
      "checkbox",
      "color",
      "file",
      "hidden",
      "image",
      "radio",
      "range",
      "reset",
      "submit",
    ].includes(type);
  }
  return el instanceof HTMLTextAreaElement;
}

function closestEditable(target: EventTarget | null): EditableTarget | null {
  if (!(target instanceof Node)) return null;
  let node: Node | null = target;
  while (node) {
    if (node instanceof HTMLElement && isEditable(node)) return node;
    node = node.parentNode;
  }
  return null;
}

function selectionText(): string {
  const value = window.getSelection()?.toString() ?? "";
  return value;
}

function canUndo(el: EditableTarget | null): boolean {
  if (!el) return false;
  try {
    return document.queryCommandEnabled("undo");
  } catch {
    return true;
  }
}

function canRedo(el: EditableTarget | null): boolean {
  if (!el) return false;
  try {
    return document.queryCommandEnabled("redo");
  } catch {
    return true;
  }
}

async function readClipboardText(): Promise<string> {
  try {
    if (navigator.clipboard?.readText) {
      return await navigator.clipboard.readText();
    }
  } catch {
    // ignore permission errors; paste may still work via execCommand
  }
  return "";
}

async function writeClipboardText(text: string): Promise<boolean> {
  try {
    if (navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(text);
      return true;
    }
  } catch {
    // fall through
  }
  try {
    return document.execCommand("copy");
  } catch {
    return false;
  }
}

function focusEditable(el: EditableTarget | null) {
  if (!el) return;
  try {
    el.focus({ preventScroll: true });
  } catch {
    el.focus();
  }
}

export function useContextMenu() {
  const visible = ref(false);
  const left = ref(0);
  const top = ref(0);
  const items = ref<ContextMenuItem[]>([]);
  const activeEditable = ref<EditableTarget | null>(null);
  const selectedText = ref("");
  const clipboardText = ref("");

  function hide() {
    visible.value = false;
    items.value = [];
    activeEditable.value = null;
  }

  function placeMenu(clientX: number, clientY: number, count: number) {
    const itemHeight = 32;
    const height = count * itemHeight + 8;
    const maxLeft = window.innerWidth - MENU_WIDTH - MENU_PAD;
    const maxTop = window.innerHeight - height - MENU_PAD;
    left.value = Math.max(MENU_PAD, Math.min(clientX, maxLeft));
    top.value = Math.max(MENU_PAD, Math.min(clientY, maxTop));
  }

  async function buildItems(editable: EditableTarget | null, text: string) {
    const clip = await readClipboardText();
    clipboardText.value = clip;
    selectedText.value = text;
    activeEditable.value = editable;

    if (editable) {
      const hasSelection =
        text.length > 0 ||
        (editable instanceof HTMLInputElement || editable instanceof HTMLTextAreaElement
          ? editable.selectionStart !== editable.selectionEnd
          : false);
      const notReadonly =
        !(editable instanceof HTMLInputElement && editable.readOnly) &&
        !(editable instanceof HTMLTextAreaElement && editable.readOnly);
      items.value = [
        { id: "undo", label: "撤销", enabled: notReadonly && canUndo(editable), accelerator: "⌘Z" },
        { id: "redo", label: "重做", enabled: notReadonly && canRedo(editable), accelerator: "⇧⌘Z" },
        { id: "sep-1", label: "", enabled: false, separator: true },
        { id: "cut", label: "剪切", enabled: notReadonly && hasSelection, accelerator: "⌘X" },
        { id: "copy", label: "拷贝", enabled: hasSelection, accelerator: "⌘C" },
        {
          id: "paste",
          label: "粘贴",
          enabled: notReadonly,
          accelerator: "⌘V",
        },
        { id: "select-all", label: "全选", enabled: true, accelerator: "⌘A" },
      ];
      return;
    }

    if (text) {
      items.value = [
        { id: "copy", label: "拷贝", enabled: true, accelerator: "⌘C" },
        { id: "sep-1", label: "", enabled: false, separator: true },
        { id: "select-all", label: "全选", enabled: true, accelerator: "⌘A" },
      ];
      return;
    }

    items.value = [{ id: "select-all", label: "全选", enabled: true, accelerator: "⌘A" }];
  }

  async function onContextMenu(event: MouseEvent) {
    // 让自定义控件/显式处理过的区域自己决定
    const path = typeof event.composedPath === "function" ? event.composedPath() : [];
    for (const node of path) {
      if (!(node instanceof HTMLElement)) continue;
      if (node.dataset?.nativeContextMenu === "true") return;
      if (node.classList?.contains("oh-context-menu")) return;
    }

    event.preventDefault();
    event.stopPropagation();

    const editable = closestEditable(event.target);
    const text = selectionText().trim() ? selectionText() : "";
    await buildItems(editable, text);
    placeMenu(event.clientX, event.clientY, items.value.filter((i) => !i.separator).length || 1);
    visible.value = true;
  }

  function onPointerDown(event: PointerEvent) {
    if (!visible.value) return;
    const target = event.target;
    if (target instanceof HTMLElement && target.closest(".oh-context-menu")) return;
    hide();
  }

  function onKeydown(event: KeyboardEvent) {
    if (!visible.value) return;
    if (event.key === "Escape") {
      event.preventDefault();
      hide();
    }
  }

  function onBlur() {
    hide();
  }

  function onScroll() {
    if (visible.value) hide();
  }

  async function runAction(id: string) {
    const editable = activeEditable.value;
    const text = selectedText.value || selectionText();
    hide();

    switch (id) {
      case "undo": {
        focusEditable(editable);
        document.execCommand("undo");
        break;
      }
      case "redo": {
        focusEditable(editable);
        document.execCommand("redo");
        break;
      }
      case "cut": {
        focusEditable(editable);
        if (!document.execCommand("cut") && text) {
          await writeClipboardText(text);
          document.execCommand("delete");
        }
        break;
      }
      case "copy": {
        if (editable) {
          focusEditable(editable);
          if (!document.execCommand("copy") && text) await writeClipboardText(text);
        } else if (text) {
          await writeClipboardText(text);
        }
        break;
      }
      case "paste": {
        focusEditable(editable);
        if (!document.execCommand("paste")) {
          const clip = clipboardText.value || (await readClipboardText());
          if (clip) document.execCommand("insertText", false, clip);
        }
        break;
      }
      case "select-all": {
        if (editable) {
          focusEditable(editable);
          if (editable instanceof HTMLInputElement || editable instanceof HTMLTextAreaElement) {
            editable.select();
          } else {
            document.execCommand("selectAll");
          }
        } else {
          document.execCommand("selectAll");
        }
        break;
      }
      default:
        break;
    }
  }

  onMounted(() => {
    document.addEventListener("contextmenu", onContextMenu, true);
    document.addEventListener("pointerdown", onPointerDown, true);
    document.addEventListener("keydown", onKeydown, true);
    window.addEventListener("blur", onBlur);
    document.addEventListener("scroll", onScroll, true);
    window.addEventListener("resize", onScroll);
  });

  onUnmounted(() => {
    document.removeEventListener("contextmenu", onContextMenu, true);
    document.removeEventListener("pointerdown", onPointerDown, true);
    document.removeEventListener("keydown", onKeydown, true);
    window.removeEventListener("blur", onBlur);
    document.removeEventListener("scroll", onScroll, true);
    window.removeEventListener("resize", onScroll);
  });

  return {
    visible,
    left,
    top,
    items,
    hide,
    runAction,
  };
}
