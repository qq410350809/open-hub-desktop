import { ref } from "vue";

// —— 全局单例 ——
const tooltipText = ref("");
const tooltipVisible = ref(false);
const tooltipLeft = ref(0);
const tooltipTop = ref(0);
const tooltipArrowLeft = ref(0);
const tooltipBelow = ref(false);

let tooltipTarget: HTMLElement | null = null;

function getTooltipText(target: HTMLElement): string {
  if (target.matches(".tag-overflow") && target.dataset.hiddenTags)
    return `已隐藏：${target.dataset.hiddenTags}`;
  if (target.dataset.tooltip) return target.dataset.tooltip;
  if (target.dataset.uiTooltip) return target.dataset.uiTooltip;
  const title = target.getAttribute("title")?.trim();
  if (title) {
    target.dataset.uiTooltip = title;
    target.removeAttribute("title");
    if (target.matches("button") && !target.getAttribute("aria-label"))
      target.setAttribute("aria-label", title);
    return title;
  }
  if (target.matches('button[aria-label],[role="button"][aria-label]'))
    return target.getAttribute("aria-label")?.trim() ?? "";
  return "";
}

function tooltipElement(origin: EventTarget | null): HTMLElement | null {
  if (!(origin instanceof Element)) return null;
  return origin.closest<HTMLElement>(
    '.tag-overflow,[data-tooltip],[data-ui-tooltip],[title],button[aria-label],[role="button"][aria-label]',
  );
}

function showTooltip(target: HTMLElement) {
  const content = getTooltipText(target);
  if (!content) return;
  tooltipTarget = target;
  tooltipText.value = content;
  tooltipVisible.value = true;
  tooltipBelow.value = false;

  requestAnimationFrame(() => {
    if (!tooltipTarget) return;
    const targetRect = tooltipTarget.getBoundingClientRect();
    const tooltipEl = document.querySelector<HTMLElement>(".ui-tooltip");
    if (!tooltipEl) return;
    const tooltipRect = tooltipEl.getBoundingClientRect();
    const left = Math.min(
      Math.max(10, targetRect.left + targetRect.width / 2 - tooltipRect.width / 2),
      window.innerWidth - tooltipRect.width - 10,
    );
    const above = targetRect.top - tooltipRect.height - 9;
    const showBelow = above < 10;
    const arrowLeft = Math.min(
      Math.max(9, targetRect.left + targetRect.width / 2 - left),
      tooltipRect.width - 9,
    );
    tooltipLeft.value = left;
    tooltipTop.value = showBelow ? targetRect.bottom + 9 : above;
    tooltipArrowLeft.value = arrowLeft;
    tooltipBelow.value = showBelow;
  });
}

function hideTooltip(target?: HTMLElement | null) {
  if (target && tooltipTarget !== target) return;
  tooltipVisible.value = false;
  tooltipTarget = null;
}

export function useTooltip() {
  function onPointerOver(event: PointerEvent) {
    const target = tooltipElement(event.target);
    if (target && !target.contains(event.relatedTarget as Node | null)) showTooltip(target);
  }
  function onPointerOut(event: PointerEvent) {
    const target = tooltipElement(event.target);
    if (target && !target.contains(event.relatedTarget as Node | null)) hideTooltip(target);
  }
  function onFocusIn(event: FocusEvent) {
    const target = tooltipElement(event.target);
    if (target) showTooltip(target);
  }
  function onFocusOut(event: FocusEvent) {
    const target = tooltipElement(event.target);
    if (target) hideTooltip(target);
  }
  function onPointerDown() {
    hideTooltip();
  }
  function onScroll() {
    hideTooltip();
  }

  return {
    tooltipText,
    tooltipVisible,
    tooltipLeft,
    tooltipTop,
    tooltipArrowLeft,
    tooltipBelow,
    onPointerOver,
    onPointerOut,
    onFocusIn,
    onFocusOut,
    onPointerDown,
    onScroll,
    hideTooltip,
  };
}
