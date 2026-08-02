<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, ref, watch } from "vue";
import { listen } from "@tauri-apps/api/event";
import { icons } from "../icons";
import { useStore } from "../composables/useStore";
import type { SyncSitesProgress } from "../types";

const store = useStore();
const closeBtnRef = ref<HTMLButtonElement>();
const logListRef = ref<HTMLOListElement>();
const isTauri = "__TAURI_INTERNALS__" in window;
let unlistenProgress: (() => void) | undefined;
let unlistenChromeProgress: (() => void) | undefined;

const scopeLabel = computed(() =>
  store.syncDialogMode.value === "models"
    ? `当前列表中的 ${store.syncDialogSiteIds.value.length} 个在用存活站点`
    : store.syncDialogMode.value === "sessions"
    ? `当前列表中的 ${store.syncDialogSiteIds.value.length} 个在用站点`
    : store.syncDialogRunaway.value ? "跑路站点" : "存活站点",
);

const dialogTitle = computed(() =>
  store.syncDialogMode.value === "models" ? "模型同步" : "同步站点",
);

const runStateLabel = computed(() => {
  switch (store.syncRunState.value) {
    case "syncing": return "正在同步";
    case "detecting": return "后台检测中";
    case "complete": return "已完成";
    case "error": return "同步失败";
    default: return "等待开始";
  }
});

function formatElapsed(milliseconds: number) {
  return `+${(milliseconds / 1000).toFixed(1)}s`;
}

onMounted(async () => {
  if (!isTauri) return;
  unlistenProgress = await listen<SyncSitesProgress>("sync-sites-progress", (event) => {
    store.receiveSyncProgress(event.payload);
  });
  unlistenChromeProgress = await listen<SyncSitesProgress>("chrome-account-sync-progress", (event) => {
    store.receiveNestedChromeSyncProgress(event.payload);
  });
});

onUnmounted(() => {
  unlistenProgress?.();
  unlistenChromeProgress?.();
  unlistenProgress = undefined;
  unlistenChromeProgress = undefined;
});

watch(
  () => store.syncDialogOpen.value,
  (open) => {
    if (open) {
      nextTick(() => closeBtnRef.value?.focus());
      document.body.classList.add("modal-open");
    } else {
      document.body.classList.remove("modal-open");
    }
  },
);

watch(
  () => store.syncLogs.value.length,
  () => {
    nextTick(() => {
      if (logListRef.value) logListRef.value.scrollTop = logListRef.value.scrollHeight;
    });
  },
);

function close() {
  store.closeSyncDialog();
}

function onBackdropClick(event: MouseEvent) {
  if (event.target === event.currentTarget) close();
}
</script>

<template>
  <Teleport to="body">
    <div
      class="link-dialog-backdrop"
      id="sync-sites-dialog"
      :hidden="!store.syncDialogOpen.value"
      @click="onBackdropClick"
    >
      <section
        class="link-dialog sync-sites-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby="sync-sites-title"
      >
        <header class="link-dialog-header">
          <div>
            <h2 id="sync-sites-title">{{ dialogTitle }}</h2>
            <p v-if="store.syncDialogMode.value === 'remote'">验证 Chrome 登录状态后同步{{ scopeLabel }}</p>
            <p v-else-if="store.syncDialogMode.value === 'sessions'">并行同步{{ scopeLabel }}的 Chrome 会话</p>
            <p v-else>同步{{ scopeLabel }}所有账号的 Key 与模型</p>
          </div>
          <button
            ref="closeBtnRef"
            class="close-button"
            type="button"
            :aria-label="`关闭${dialogTitle}窗口`"
            :disabled="store.syncingSites.value || store.syncingModelKeys.value"
            @click="close"
            v-html="icons.close"
          />
        </header>

        <div class="sync-sites-content">
          <div v-if="store.syncDialogMode.value === 'remote' && store.remoteUserLoading.value" class="sync-sites-state" aria-live="polite">
            <span class="sync-sites-state-icon is-loading" v-html="icons.restore" />
            <strong>正在读取 Chrome 登录状态…</strong>
            <p>正在从本机 Chrome 获取 ldoh 会话并验证当前用户。</p>
          </div>

          <div v-else-if="store.syncDialogMode.value === 'remote' && !store.remoteUser.value" class="sync-sites-state" aria-live="polite">
            <span class="sync-sites-state-icon is-error" v-html="icons.user" />
            <strong>暂未获取到登录用户</strong>
            <p v-if="store.remoteUserError.value" class="sync-sites-error">
              {{ store.remoteUserError.value }}
            </p>
            <p v-else>请先在 Chrome 中登录 ldoh，然后返回此窗口重新获取。</p>
            <div class="sync-sites-state-actions">
              <button class="primary-button" type="button" @click="store.openRemoteLogin()">
                <span v-html="icons.external" />
                <span>前往浏览器登录</span>
              </button>
              <button
                class="secondary-button"
                type="button"
                :disabled="store.remoteUserLoading.value"
                @click="store.refreshRemoteUser()"
              >
                <span v-html="icons.restore" />
                <span>重新获取</span>
              </button>
            </div>
          </div>

          <template v-else>
            <div v-if="store.syncDialogMode.value === 'remote' && store.remoteUser.value" class="sync-user-card">
              <span class="sync-user-avatar" v-html="icons.user" />
              <div class="sync-user-details">
                <strong>{{ store.remoteUser.value?.name }}</strong>
                <span v-if="store.remoteUser.value?.username">
                  @{{ store.remoteUser.value?.username }}
                </span>
                <small>
                  Chrome {{ store.remoteUser.value?.profileName }}
                  <template v-if="store.remoteUser.value?.accountName">
                    · {{ store.remoteUser.value?.accountName }}
                  </template>
                </small>
              </div>
              <span class="sync-user-status">已登录</span>
            </div>

            <div class="sync-scope-row">
              <span
                class="sync-scope-icon"
                v-html="store.syncDialogMode.value === 'models'
                  ? icons.cpu
                  : store.syncDialogRunaway.value ? icons.flag : icons.globe"
              />
              <div>
                <strong>本次同步范围</strong>
                <p v-if="store.syncDialogMode.value === 'remote'">{{ scopeLabel }} · 同名远端记录会更新，本地在用状态会保留</p>
                <p v-else-if="store.syncDialogMode.value === 'sessions'">{{ scopeLabel }} · 仅更新这些站点的本地账号缓存</p>
                <p v-else>{{ scopeLabel }} · 仅同步这些站点下合法账号的 Key 与模型</p>
              </div>
            </div>

            <div
              v-if="store.syncDialogMode.value === 'models' && store.syncingModelKeys.value"
              class="model-sync-loading"
              role="status"
              aria-live="polite"
            >
              <span class="model-sync-loading-icon" v-html="icons.restore" />
              <div class="model-sync-loading-content">
                <header>
                  <strong>正在同步 Key 与模型</strong>
                  <span>
                    {{ store.modelKeySyncCompleted.value }}/{{ store.modelKeySyncTotal.value }} 个账号
                  </span>
                </header>
                <p>正在读取账号 Key，并验证在线模型列表，请稍候。</p>
                <progress
                  aria-label="Key 与模型同步进度"
                  :max="store.modelKeySyncTotal.value || 1"
                  :value="store.modelKeySyncCompleted.value"
                />
              </div>
            </div>

            <section
              v-if="store.syncRunState.value !== 'idle'"
              class="sync-log-panel"
              aria-label="同步日志"
              aria-live="polite"
            >
              <header>
                <div>
                  <strong>同步日志</strong>
                  <span :class="`is-${store.syncRunState.value}`">{{ runStateLabel }}</span>
                </div>
                <time>{{ formatElapsed(store.syncElapsedMs.value) }}</time>
              </header>
              <ol ref="logListRef">
                <li
                  v-for="entry in store.syncLogs.value"
                  :key="entry.id"
                  :class="`is-${entry.status}`"
                >
                  <i aria-hidden="true" />
                  <time>{{ formatElapsed(entry.elapsedMs) }}</time>
                  <span>{{ entry.message }}</span>
                </li>
              </ol>
            </section>

            <p v-if="store.remoteUserError.value" class="sync-sites-inline-error" role="alert">
              {{ store.remoteUserError.value }}
            </p>
          </template>
        </div>

        <footer v-if="store.syncDialogMode.value !== 'remote' || store.remoteUser.value" class="sync-sites-footer">
          <template v-if="store.syncRunState.value === 'idle'">
            <button
              v-if="store.syncDialogMode.value === 'remote'"
              class="secondary-button"
              type="button"
              :disabled="store.remoteUserLoading.value"
              @click="store.refreshRemoteUser()"
            >
              <span v-html="icons.restore" />
              <span>重新获取</span>
            </button>
            <button class="primary-button" type="button" @click="store.syncSites()">
              <span v-html="store.syncDialogMode.value === 'models' ? icons.cpu : icons.restore" />
              <span v-if="store.syncDialogMode.value === 'models'">同步 Key 与模型</span>
              <span v-else>{{ store.syncDialogMode.value === 'sessions' ? '同步当前列表' : `同步${scopeLabel}` }}</span>
            </button>
          </template>
          <template v-else>
            <div class="sync-footer-progress" :class="`is-${store.syncRunState.value}`">
              <i aria-hidden="true" />
              <span>{{ runStateLabel }}</span>
            </div>
            <button
              v-if="store.syncRunState.value === 'error'"
              class="secondary-button"
              type="button"
              @click="store.syncSites()"
            >重新同步</button>
            <button
              class="primary-button"
              type="button"
              :disabled="store.syncingSites.value || store.syncingModelKeys.value"
              :aria-busy="store.syncingSites.value || store.syncingModelKeys.value"
              @click="close"
            >
              <span v-if="store.syncingSites.value || store.syncingModelKeys.value">同步中…</span>
              <span v-else-if="store.syncRunState.value === 'detecting'">关闭（后台继续）</span>
              <span v-else>完成</span>
            </button>
          </template>
        </footer>
      </section>
    </div>
  </Teleport>
</template>
