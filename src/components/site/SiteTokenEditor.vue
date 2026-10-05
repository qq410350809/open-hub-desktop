<script setup lang="ts">
/**
 * 账号级「站点令牌」维护入口。
 *
 * 自动同步取不到登录凭据时（站点改了登录流程、浏览器里已退出、Local Storage
 * 被清），用户可以自己把站点后台的访问令牌贴进来，之后的账号 / Key 同步就直接
 * 用它鉴权。令牌与用户 ID 都能清空，清空后恢复成完全依赖浏览器会话。
 *
 * 做成按钮 + 就地展开的表单而不是弹窗：它属于某一个账号的行内操作，弹窗会
 * 让人失去「我在改哪个账号」的上下文。
 */
import { ref, computed, nextTick } from "vue";
import { icons } from "../../icons";
import { runCommand } from "../../composables/useLibrary";
import { useToast } from "../../composables/useToast";

const props = withDefaults(
  defineProps<{
    siteId: string;
    profileId: string;
    /** 账号展示名，用于提示文案里说清在改哪个账号。 */
    accountLabel: string;
    /** 该账号当前是否已有令牌：决定按钮的初始强调与标题。 */
    hasToken?: boolean;
    /** 紧凑模式：只显示钥匙图标（图标按钮组内用）。 */
    compact?: boolean;
  }>(),
  { hasToken: false, compact: false },
);

const emit = defineEmits<{ (event: "saved", hasToken: boolean): void }>();

const { showToast } = useToast();

const open = ref(false);
const token = ref("");
const userId = ref("");
const saving = ref(false);
const loading = ref(false);
const revealed = ref(false);
const tokenInputRef = ref<HTMLInputElement | null>(null);

const canSave = computed(() => token.value.trim().length > 0);

/** 打开时回填已保存的令牌：空输入框 + 「保存」会清掉原值，这是最容易被误操作毁掉的字段。 */
async function openForm() {
  if (open.value) {
    open.value = false;
    return;
  }
  open.value = true;
  revealed.value = false;
  loading.value = true;
  try {
    const value = await runCommand<{ token: string; userId: string }>("get_site_account_token", {
      siteId: props.siteId,
      profileId: props.profileId,
    });
    token.value = value?.token ?? "";
    userId.value = value?.userId ?? "";
  } catch {
    token.value = "";
    userId.value = "";
    showToast("读取已保存的令牌失败，请手动填写", true);
  } finally {
    loading.value = false;
    void nextTick(() => tokenInputRef.value?.focus());
  }
}

async function save() {
  if (saving.value || !canSave.value) return;
  saving.value = true;
  try {
    await runCommand("set_site_account_token", {
      siteId: props.siteId,
      profileId: props.profileId,
      token: token.value.trim(),
      userId: userId.value.trim(),
    });
    showToast(`已保存「${props.accountLabel}」的站点令牌，下次同步生效`);
    open.value = false;
    emit("saved", true);
  } catch (error) {
    showToast(`保存站点令牌失败：${String(error)}`, true);
  } finally {
    saving.value = false;
  }
}

async function clearToken() {
  if (saving.value) return;
  saving.value = true;
  try {
    await runCommand("set_site_account_token", {
      siteId: props.siteId,
      profileId: props.profileId,
      token: "",
      userId: "",
    });
    showToast(`已清除「${props.accountLabel}」的站点令牌`);
    open.value = false;
    emit("saved", false);
  } catch (error) {
    showToast(`清除站点令牌失败：${String(error)}`, true);
  } finally {
    saving.value = false;
  }
}
</script>

<template>
  <span class="site-token-editor">
    <button
      type="button"
      class="site-token-trigger"
      :class="{ 'is-compact': compact, 'has-token': hasToken }"
      :title="hasToken ? `「${accountLabel}」已配置站点令牌，点击可查看或替换` : `为「${accountLabel}」设置站点令牌`"
      @click.stop="openForm"
    >
      <span v-html="icons.key" />
      <span v-if="!compact" class="site-token-trigger-label">站点令牌</span>
    </button>

    <span v-if="open" class="site-token-form" @click.stop>
      <span class="site-token-form-title">
        「{{ accountLabel }}」的站点令牌
        <small v-if="hasToken">已配置，保存新值即替换</small>
      </span>
      <span class="site-token-input-row">
        <input
          ref="tokenInputRef"
          v-model="token"
          :type="revealed ? 'text' : 'password'"
          class="site-token-input"
          :placeholder="loading ? '正在读取已保存的令牌…' : '访问令牌（站点后台 → 个人设置 → 安全设置）'"
          autocomplete="off"
          spellcheck="false"
          @keydown.enter.prevent="save"
          @keydown.esc.stop="open = false"
        />
        <button
          type="button"
          class="site-token-eye"
          :title="revealed ? '隐藏令牌' : '显示令牌'"
          :aria-label="revealed ? '隐藏令牌' : '显示令牌'"
          :disabled="!token"
          @click="revealed = !revealed"
        >
          <span v-html="revealed ? icons.eyeOff : icons.eye" />
        </button>
      </span>
      <input
        v-model="userId"
        type="text"
        class="site-token-input"
        placeholder="用户 ID（可选，令牌鉴权时一般留空）"
        autocomplete="off"
        spellcheck="false"
        @keydown.enter.prevent="save"
        @keydown.esc.stop="open = false"
      />
      <span class="site-token-form-actions">
        <button type="button" class="site-token-btn" :disabled="!canSave || saving" @click="save">
          保存令牌
        </button>
        <button
          v-if="hasToken"
          type="button"
          class="site-token-btn is-danger"
          :disabled="saving"
          @click="clearToken"
        >
          清除
        </button>
        <button type="button" class="site-token-btn is-ghost" :disabled="saving" @click="open = false">
          取消
        </button>
      </span>
    </span>
  </span>
</template>

<style scoped>
.site-token-editor { position: relative; display: inline-flex; }

.site-token-trigger {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  padding: 2px 7px;
  border: 1px solid var(--line);
  border-radius: 6px;
  background: var(--surface-soft);
  color: var(--muted);
  font-size: 10.5px;
  line-height: 1.6;
  cursor: pointer;
}
.site-token-trigger :deep(svg) { width: 12px; height: 12px; }
.site-token-trigger.is-compact { width: 26px; height: 26px; padding: 0; justify-content: center; }
.site-token-trigger.has-token { color: var(--success); border-color: color-mix(in srgb, var(--success) 35%, transparent); }
.site-token-trigger:hover { color: var(--text); border-color: var(--line-strong); }

.site-token-form {
  position: absolute;
  top: calc(100% + 4px);
  right: 0;
  z-index: 40;
  display: flex;
  flex-direction: column;
  gap: 6px;
  width: 268px;
  padding: 9px 10px;
  border: 1px solid var(--line-strong);
  border-radius: 10px;
  background: var(--surface);
  box-shadow: 0 10px 28px rgba(0, 0, 0, 0.18);
}
.site-token-form-title { display: flex; align-items: baseline; gap: 6px; color: var(--text); font-size: 11.5px; font-weight: 600; }
.site-token-form-title small { color: var(--faint); font-size: 10px; font-weight: 500; }

.site-token-input-row { position: relative; display: flex; align-items: center; }
.site-token-input-row .site-token-input { padding-right: 28px; }

.site-token-eye {
  position: absolute;
  right: 4px;
  width: 22px;
  height: 22px;
  padding: 0;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  border: none;
  border-radius: 5px;
  background: transparent;
  color: var(--faint);
  cursor: pointer;
}
.site-token-eye:disabled { opacity: 0.4; cursor: not-allowed; }
.site-token-eye :deep(svg) { width: 14px; height: 14px; }
.site-token-eye:not(:disabled):hover { color: var(--text); }

.site-token-input {
  width: 100%;
  padding: 5px 7px;
  border: 1px solid var(--line);
  border-radius: 6px;
  background: var(--surface-soft);
  color: var(--text);
  font-size: 11.5px;
  font-family: var(--font-mono, monospace);
}
.site-token-input:focus { outline: none; border-color: var(--brand); }

.site-token-form-actions { display: flex; gap: 6px; }
.site-token-btn {
  padding: 4px 9px;
  border: 1px solid var(--line);
  border-radius: 6px;
  background: var(--surface-soft);
  color: var(--text);
  font-size: 11px;
  cursor: pointer;
}
.site-token-btn:disabled { opacity: 0.5; cursor: not-allowed; }
.site-token-btn.is-danger { color: var(--danger); border-color: color-mix(in srgb, var(--danger) 35%, transparent); }
.site-token-btn.is-ghost { color: var(--muted); }
</style>