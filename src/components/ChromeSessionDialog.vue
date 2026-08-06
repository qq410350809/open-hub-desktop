<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, ref, watch } from "vue";
import { listen } from "@tauri-apps/api/event";
import { icons } from "../icons";
import { useStore } from "../composables/useStore";
import type { ChromeSessionInfo, SyncSitesProgress } from "../types";

const store = useStore();
const closeBtnRef = ref<HTMLButtonElement>();
const logListRef = ref<HTMLOListElement>();
const isTauri = "__TAURI_INTERNALS__" in window;

const browserSyncStateLabel = computed(() => {
  if (store.chromeSessionsLoading.value || store.chromeUsageScanning.value) return "正在扫描";
  if (store.chromeBrowserSyncingProfileId.value) return "正在同步";
  if (store.chromeModelsSyncing.value) return "同步 Key 与模型";
  if (store.chromeBrowserSyncError.value) return "同步失败";
  return "已完成";
});

function formatElapsed(milliseconds: number) {
  if (milliseconds < 1000) {
    return `+${milliseconds}ms`;
  }
  return `+${(milliseconds / 1000).toFixed(1)}s`;
}

let isDialogMounted = true;
let unlistenChromeProgress: (() => void) | undefined;
let unlistenSyncProgress: (() => void) | undefined;

onMounted(() => {
  if (!isTauri) return;
  isDialogMounted = true;
  listen<SyncSitesProgress>("chrome-account-sync-progress", (event) => {
    store.receiveChromeBrowserSyncProgress(event.payload);
  }).then((unlisten) => {
    if (!isDialogMounted) unlisten();
    else unlistenChromeProgress = unlisten;
  });
  listen<SyncSitesProgress>("sync-sites-progress", (event) => {
    store.receiveChromeBrowserSyncProgress(event.payload);
  }).then((unlisten) => {
    if (!isDialogMounted) unlisten();
    else unlistenSyncProgress = unlisten;
  });
});

onUnmounted(() => {
  isDialogMounted = false;
  unlistenChromeProgress?.();
  unlistenChromeProgress = undefined;
  unlistenSyncProgress?.();
  unlistenSyncProgress = undefined;
});

watch(
  () => store.chromeSessionDialogOpen.value,
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
  () => store.chromeBrowserSyncLogs.value.length,
  () => {
    nextTick(() => {
      if (logListRef.value) logListRef.value.scrollTop = logListRef.value.scrollHeight;
    });
  },
);

function close() {
  store.closeChromeSessionDialog();
}

function onBackdropClick(event: MouseEvent) {
  if (event.target === event.currentTarget) close();
}

async function copySession(session: ChromeSessionInfo) {
  await store.copyChromeSession(session);
}

async function syncViaChrome(session: ChromeSessionInfo) {
  await store.syncAccountViaChrome(session);
}
</script>

<template>
  <Teleport to="body">
    <div
      class="link-dialog-backdrop"
      id="chrome-session-dialog"
      :hidden="!store.chromeSessionDialogOpen.value"
      @click="onBackdropClick"
    >
      <section
        class="link-dialog chrome-session-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby="chrome-session-title"
      >
        <header class="link-dialog-header">
          <div>
            <h2 id="chrome-session-title">Chrome 账号会话</h2>
            <p>{{ store.chromeSessionSite.value?.name || "站点" }}</p>
          </div>
          <button
            ref="closeBtnRef"
            class="close-button"
            type="button"
            aria-label="关闭 Chrome 账号会话"
            :disabled="Boolean(store.chromeBrowserSyncingProfileId.value) || store.chromeModelsSyncing.value"
            @click="close"
            v-html="icons.close"
          />
        </header>

       <div class="chrome-session-list">
          <section
            v-if="store.chromeBrowserSyncLogs.value.length"
            class="sync-log-panel chrome-sync-log-panel"
            aria-label="Chrome 账号执行日志"
            aria-live="polite"
          >
            <header>
              <div>
                <strong>执行日志</strong>
                <span
                  :class="{
                    'is-error': Boolean(store.chromeBrowserSyncError.value),
                    'is-running': store.chromeSessionsLoading.value
                      || store.chromeUsageScanning.value
                      || Boolean(store.chromeBrowserSyncingProfileId.value)
                      || store.chromeModelsSyncing.value,
                    'is-complete': !store.chromeSessionsLoading.value
                      && !store.chromeUsageScanning.value
                      && !store.chromeBrowserSyncingProfileId.value
                      && !store.chromeModelsSyncing.value
                      && !store.chromeBrowserSyncError.value,
                  }"
                >{{ browserSyncStateLabel }}</span>
              </div>
              <time>{{ formatElapsed(store.chromeBrowserSyncElapsedMs.value) }}</time>
            </header>
           <ol ref="logListRef">
             <li
               v-for="entry in store.chromeBrowserSyncLogs.value"
               :key="entry.id"
               :class="`is-${entry.status}`"
             >
               <i aria-hidden="true" />
               <time>{{ formatElapsed(entry.elapsedMs) }}</time>
               <span>{{ entry.message }}</span>
             </li>
           </ol>
         </section>

          <div
            v-if="store.chromeSessionsLoading.value"
            class="chrome-session-scanning"
            role="status"
            aria-live="polite"
          >
            <span class="chrome-session-scanning-icon" v-html="icons.restore" />
            <span>正在扫描本机 Chrome 配置…</span>
          </div>
          <div v-if="store.chromeSessionsError.value && !store.chromeSessionsLoading.value" class="chrome-session-state error">
            {{ store.chromeSessionsError.value }}
          </div>
          <template v-if="!store.chromeSessionsLoading.value && !store.chromeSessionsError.value">
            <div v-if="store.chromeBrowserSyncError.value" class="chrome-browser-sync-error" role="alert">
              {{ store.chromeBrowserSyncError.value }}
            </div>
            <article
              v-for="session in store.chromeSessions.value"
              :key="session.profileId"
              class="chrome-session-row"
            >
            <div class="chrome-session-avatar" v-html="icons.user" />
            <div class="chrome-session-details">
              <strong>{{ session.profileName }}</strong>
              <small>{{ session.accountName || session.profileId }} · {{ session.domain }}</small>
              <div class="chrome-cookie-names" :title="session.cookieNames.join(', ')">
                {{ session.cookieNames.slice(0, 5).join(" · ") }}
                <span v-if="session.cookieNames.length > 5">+{{ session.cookieNames.length - 5 }}</span>
              </div>
              <small v-if="session.syncError" class="chrome-account-warning" :title="session.syncError">
                {{ session.syncError }}
              </small>
            </div>
            <span class="chrome-cookie-count">{{ session.cookieCount }} 个</span>
            <div class="chrome-session-actions">
              <button
                v-if="store.canSyncAccountViaChrome(session)"
                class="chrome-browser-sync"
                type="button"
                :aria-label="`使用 Chrome 同步 ${session.profileName}`"
                title="使用 Chrome 同步"
                :disabled="Boolean(store.chromeBrowserSyncingProfileId.value) || store.chromeModelsSyncing.value"
                @click="syncViaChrome(session)"
                v-html="icons.globe"
              />
              <button
                class="copy-address"
                type="button"
                :aria-label="`复制 ${session.profileName} 的会话`"
                title="复制会话"
                :disabled="store.chromeSessionCopyingProfileId.value === session.profileId || Boolean(store.chromeBrowserSyncingProfileId.value) || store.chromeModelsSyncing.value"
                @click="copySession(session)"
                v-html="icons.copy"
              />
            </div>
            </article>
          </template>
        </div>
      </section>
    </div>
  </Teleport>
</template>
