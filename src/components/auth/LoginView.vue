<script setup lang="ts">
import { ref } from "vue";
import { runCommand } from "../../composables/useLibrary";
import { setSessionToken } from "../../composables/core/ipc";

const props = defineProps<{
  /** 预填用户名（来自后端配置） */
  hintUsername?: string;
}>();

const emit = defineEmits<{
  (e: "authenticated", token: string): void;
}>();

const username = ref(props.hintUsername || "admin");
const password = ref("");
const submitting = ref(false);
const errorMessage = ref("");

async function submit() {
  if (submitting.value) return;
  const user = username.value.trim();
  if (!user || !password.value) {
    errorMessage.value = "请输入用户名和密码";
    return;
  }
  submitting.value = true;
  errorMessage.value = "";
  try {
    const token = await runCommand<string>("login", {
      username: user,
      password: password.value,
    });
    setSessionToken(String(token ?? ""));
    emit("authenticated", String(token ?? ""));
  } catch (error) {
    errorMessage.value = typeof error === "string" ? error : (error as Error)?.message ?? "登录失败";
  } finally {
    submitting.value = false;
  }
}
</script>

<template>
  <div class="login-gate" role="main" aria-label="OpenHub 登录">
    <div class="login-card">
      <div class="login-brand">
        <div class="login-logo">OH</div>
        <h1 class="login-title">OpenHub</h1>
        <p class="login-subtitle">本地站点资料库 · 模型网关 · Token 统计</p>
      </div>

      <form class="login-form" @submit.prevent="submit">
        <label class="login-field">
          <span class="login-label">用户名</span>
          <input
            v-model="username"
            type="text"
            name="username"
            autocomplete="username"
            spellcheck="false"
            placeholder="请输入用户名"
          />
        </label>
        <label class="login-field">
          <span class="login-label">密码</span>
          <input
            v-model="password"
            type="password"
            name="password"
            autocomplete="current-password"
            placeholder="请输入密码"
          />
        </label>

        <p v-if="errorMessage" class="login-error" role="alert">{{ errorMessage }}</p>

        <button type="submit" class="login-submit" :disabled="submitting">
          {{ submitting ? "正在验证…" : "登 录" }}
        </button>
      </form>

      <p class="login-footnote">默认账号 admin / Admin@2026，可在服务启动参数中修改</p>
    </div>
  </div>
</template>

<style scoped>
.login-gate {
  position: fixed;
  inset: 0;
  z-index: 9999;
  display: flex;
  align-items: center;
  justify-content: center;
  background:
    radial-gradient(1200px 600px at 20% -10%, rgba(255, 177, 3, 0.12), transparent 60%),
    radial-gradient(900px 500px at 110% 110%, rgba(84, 158, 255, 0.1), transparent 55%),
    var(--background-color, #f5f6f8);
}

.login-card {
  width: min(380px, calc(100vw - 48px));
  padding: 40px 36px 28px;
  border-radius: 16px;
  background: var(--panel-color, #fff);
  border: 1px solid var(--border-color, #e5e7eb);
  box-shadow: 0 24px 64px rgba(15, 23, 42, 0.14);
}

.login-brand {
  text-align: center;
  margin-bottom: 26px;
}

.login-logo {
  width: 56px;
  height: 56px;
  margin: 0 auto 14px;
  display: grid;
  place-items: center;
  border-radius: 16px;
  font-weight: 800;
  font-size: 20px;
  letter-spacing: 0.5px;
  color: #1f2937;
  background: linear-gradient(135deg, #ffb103, #ffd76a);
  box-shadow: inset 0 0 0 1px rgba(0, 0, 0, 0.06);
}

.login-title {
  margin: 0;
  font-size: 22px;
  color: var(--text-color, #111827);
}

.login-subtitle {
  margin: 6px 0 0;
  font-size: 12px;
  color: var(--text-secondary, #6b7280);
}

.login-form {
  display: flex;
  flex-direction: column;
  gap: 14px;
}

.login-field {
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.login-label {
  font-size: 12px;
  font-weight: 600;
  color: var(--text-secondary, #4b5563);
}

.login-field input {
  height: 40px;
  padding: 0 12px;
  border-radius: 10px;
  border: 1px solid var(--border-color, #d1d5db);
  background: var(--input-bg, #fff);
  color: var(--text-color, #111827);
  font-size: 14px;
  outline: none;
  transition: border-color 0.15s ease, box-shadow 0.15s ease;
}

.login-field input:focus {
  border-color: #ffb103;
  box-shadow: 0 0 0 3px rgba(255, 177, 3, 0.18);
}

.login-error {
  margin: 0;
  font-size: 12px;
  color: #dc2626;
}

.login-submit {
  height: 42px;
  margin-top: 4px;
  border: none;
  border-radius: 10px;
  font-size: 15px;
  font-weight: 700;
  letter-spacing: 4px;
  color: #211a05;
  background: linear-gradient(135deg, #ffb103, #ffca45);
  cursor: pointer;
  transition: filter 0.15s ease, transform 0.05s ease;
}

.login-submit:hover:not(:disabled) {
  filter: brightness(1.05);
}

.login-submit:active:not(:disabled) {
  transform: translateY(1px);
}

.login-submit:disabled {
  opacity: 0.65;
  cursor: not-allowed;
}

.login-footnote {
  margin: 18px 0 0;
  text-align: center;
  font-size: 11px;
  color: var(--text-tertiary, #9ca3af);
}
</style>
