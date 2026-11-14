<script setup lang="ts">
import { computed, nextTick, ref } from "vue";
import { useStore } from "../../composables/useStore";
import type { SiteRecord } from "../../types";

const props = defineProps<{ site: SiteRecord }>();
const store = useStore();
const configured = computed(() => Boolean(props.site.auxBaseUrl?.trim()));
const dialog = ref<HTMLDialogElement | null>(null);
const trigger = ref<HTMLButtonElement | null>(null);
const input = ref<HTMLInputElement | null>(null);
const address = ref("");
const error = ref("");
const saving = ref(false);

async function open() {
  address.value = props.site.auxBaseUrl ?? "";
  error.value = "";
  dialog.value?.showModal();
  await nextTick();
  input.value?.focus();
}

function close() {
  if (saving.value) return;
  dialog.value?.close();
  trigger.value?.focus();
}

async function save() {
  if (saving.value) return;
  const value = address.value.trim();
  error.value = "";
  if (value) {
    try {
      const parsed = new URL(value);
      if (!["http:", "https:"].includes(parsed.protocol) || !parsed.hostname) throw new Error();
    } catch {
      error.value = "请输入完整的 http:// 或 https:// 地址，留空可清除。";
      input.value?.focus();
      return;
    }
  }
  saving.value = true;
  try {
    await store.saveAuxAddress(props.site.id, value);
    saving.value = false;
    close();
  } catch (cause) {
    error.value = String(cause);
  } finally {
    saving.value = false;
  }
}

function viewAddresses() {
  close();
  if (trigger.value) store.openLinkDialog(props.site, "api", trigger.value);
}
</script>

<template>
  <button
    ref="trigger"
    class="site-aux-trigger"
    :class="{ 'is-configured': configured }"
    type="button"
    :title="configured ? `编辑辅助站点地址：${site.auxBaseUrl}` : '设置辅助站点地址'"
    :aria-label="`${site.name}：${configured ? '编辑' : '设置'}辅助站点地址`"
    @click.stop="open"
  >{{ configured ? '辅助地址' : '+ 辅助地址' }}</button>
  <Teleport to="body">
    <dialog
      ref="dialog"
      class="site-aux-dialog"
      :aria-labelledby="`site-aux-title-${site.id}`"
      @cancel.prevent="close"
      @click="($event.target === dialog) && close()"
    >
      <form class="site-aux-form" novalidate @submit.prevent="save">
        <h2 :id="`site-aux-title-${site.id}`">辅助站点地址</h2>
        <p>{{ site.name }}</p>
        <label>
          <span>辅助地址</span>
          <input
            ref="input"
            v-model="address"
            type="url"
            name="auxBaseUrl"
            placeholder="https://backup.example.com"
            autocomplete="url"
            spellcheck="false"
            :disabled="saving"
            :aria-invalid="Boolean(error)"
            :aria-describedby="error ? `site-aux-error-${site.id}` : undefined"
          />
        </label>
        <small>填写备用域名或镜像站的完整地址；留空并保存可清除。</small>
        <p v-if="error" :id="`site-aux-error-${site.id}`" class="site-aux-error" role="alert">{{ error }}</p>
        <div class="site-aux-actions">
          <button v-if="configured" type="button" :disabled="saving" @click="viewAddresses">查看地址</button>
          <button type="button" :disabled="saving" @click="close">取消</button>
          <button class="site-aux-save" type="submit" :disabled="saving">{{ saving ? '保存中…' : '保存' }}</button>
        </div>
      </form>
    </dialog>
  </Teleport>
</template>

<style scoped>
.site-aux-trigger { flex-shrink: 0; padding: 4px 8px; border: 1px solid var(--line); border-radius: 7px; background: var(--surface-soft); color: var(--muted); font: inherit; font-size: 11px; white-space: nowrap; cursor: pointer; }
.site-aux-trigger:hover, .site-aux-trigger.is-configured { color: var(--text); border-color: var(--line-strong); }
.site-aux-dialog { width: min(420px, calc(100vw - 32px)); padding: 0; border: 1px solid var(--line-strong); border-radius: 14px; background: var(--surface); color: var(--text); box-shadow: 0 20px 60px #0004; }
.site-aux-dialog::backdrop { background: #0006; }
.site-aux-form { display: flex; flex-direction: column; gap: 12px; padding: 22px; }
.site-aux-form h2 { margin: 0; font-size: 18px; }
.site-aux-form p { margin: 0; overflow-wrap: anywhere; }
.site-aux-form label { display: flex; flex-direction: column; gap: 6px; }
.site-aux-form input { width: 100%; box-sizing: border-box; padding: 10px; border: 1px solid var(--line-strong); border-radius: 8px; background: var(--surface-soft); color: var(--text); }
.site-aux-form small, .site-aux-form > p { color: var(--muted); }
.site-aux-form .site-aux-error { color: var(--danger, #d44); font-size: 12px; }
.site-aux-actions { display: flex; justify-content: flex-end; gap: 8px; margin-top: 4px; }
.site-aux-actions button { padding: 7px 12px; border: 1px solid var(--line-strong); border-radius: 7px; background: var(--surface-soft); color: var(--text); cursor: pointer; }
.site-aux-actions .site-aux-save { font-weight: 600; }
.site-aux-actions button:disabled { opacity: .6; cursor: wait; }
</style>
