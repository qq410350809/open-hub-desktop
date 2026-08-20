<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, ref } from "vue";
import { icons } from "../icons";
import { formatDate, formatRateLimit, logoText } from "../utils";
import { useStore } from "../composables/useStore";
import TagList from "./TagList.vue";
import type { ChromeSessionInfo, SiteRecord } from "../types";
import { normalizeSystemType, systemTypeLabel } from "../types";

const props = defineProps<{
  site: SiteRecord;
}>();

const store = useStore();

const rateLimit = computed(() => formatRateLimit(props.site.rateLimit));
const logo = computed(() => logoText(props.site.apiBaseUrl, props.site.name));
const personalMode = computed(() => store.usageFilter.value === "personal");
const pendingMode = computed(() => store.usageFilter.value === "pending");
// “在用”保留原有账号卡片；“待定”只单向复用它的布局与操作区。
const usageCardMode = computed(() => personalMode.value || pendingMode.value);
const accountSessions = computed(() =>
  (store.chromeUsageAccounts.value[props.site.id] ?? [])
    .filter((session) => session.isValid)
    .slice()
    .sort((a, b) =>
      (a.username || a.accountName || a.profileName || "").localeCompare(
        b.username || b.accountName || b.profileName || "",
        undefined,
        { numeric: true, sensitivity: "base" }
      )
    ),
);
const displayedUpdatedAt = computed(() => {
  if (!usageCardMode.value) return props.site.updatedAt;
  return accountSessions.value.reduce((latest, session) => {
    if (!session.accountUpdatedAt) return latest;
    if (!latest) return session.accountUpdatedAt;
    return new Date(session.accountUpdatedAt).getTime() > new Date(latest).getTime()
      ? session.accountUpdatedAt
      : latest;
  }, "") || props.site.updatedAt;
});

const extensionDetails = computed(() =>
  props.site.extensionLinks
    .filter((link) => link.url.trim())
    .map((link) => `${link.label.trim() || "扩展链接"}：${link.url.trim()}`),
);

function configuredLinkButton(
  iconSvg: string,
  title: string,
  details: string[],
): { show: boolean; tooltip: string; iconSvg: string } | null {
  const configured = details.map((d) => d.trim()).filter(Boolean);
  if (!configured.length) return null;
  return { show: true, tooltip: [title, ...configured].join("\n"), iconSvg };
}

const linkButtons = computed(() => {
  const api = configuredLinkButton(icons.link, "API 地址", [props.site.apiBaseUrl]);
  const checkin = configuredLinkButton(
    icons.calendar,
    "签到地址",
    props.site.checkinUrl ? [props.site.checkinNote, props.site.checkinUrl] : [],
  );
  const benefit = configuredLinkButton(icons.gift, "福利站地址", [props.site.benefitUrl]);
  const status = configuredLinkButton(icons.pulse, "状态页地址", [props.site.statusUrl]);
  const extension = configuredLinkButton(icons.more, "扩展链接", extensionDetails.value);
  return { api, checkin, benefit, status, extension };
});

const capabilities = computed(() => [
  props.site.supportsImmersiveTranslation ? { icon: icons.translate, title: "沉浸式翻译" } : null,
  props.site.supportsLdc ? { icon: icons.card, title: "LDC" } : null,
  props.site.supportsNsfw ? { icon: "", title: "NSFW", ageChip: true } : null,
].filter((item): item is NonNullable<typeof item> => item !== null));

function accountIdentity(session: ChromeSessionInfo): string {
  return session.accountName || session.profileName;
}

function accountQuota(session: ChromeSessionInfo): string {
  if (session.remaining === null || !Number.isFinite(session.remaining)) return "未读取";
  const amount = new Intl.NumberFormat("zh-CN", {
    maximumFractionDigits: 2,
  }).format(session.remaining);
  return session.unit ? `${amount} ${session.unit}` : amount;
}

// —— ⋯ 更多操作菜单 ——
const menuOpen = ref(false);
const menuRoot = ref<HTMLElement | null>(null);
const menuTrigger = ref<HTMLButtonElement | null>(null);
const menuPopup = ref<HTMLElement | null>(null);

interface CardMenuEntry {
  key: string;
  label: string;
  icon: string;
  danger?: boolean;
  success?: boolean;
  separatorBefore?: boolean;
}

const cardMenuId = computed(() => `card-menu-${props.site.id}`);

// 使用状态三选一：只展示除当前状态外的两个可切换目标，不显示当前状态自身。
const usageTargets = computed<CardMenuEntry[]>(() => {
  const { isPersonal, isPending } = props.site;
  return [
    { key: "usage-personal", label: "转换为在用", icon: icons.bookmark, show: !isPersonal },
    { key: "usage-pending", label: "转换为待定", icon: icons.clock, show: !isPending },
    {
      key: "usage-unused",
      label: "转换为未在用",
      icon: icons.eyeOff,
      show: isPersonal || isPending,
    },
  ]
    .filter((entry) => entry.show)
    .map(({ show: _show, ...entry }) => entry);
});

const menuEntries = computed<CardMenuEntry[]>(() => {
  const isUsage = usageCardMode.value;
  const entries: CardMenuEntry[] = [
    { key: "models", label: "查看模型", icon: icons.cpu },
    { key: "preview", label: "查看详情", icon: icons.info },
    { key: "edit", label: "编辑", icon: icons.edit },
    ...(isUsage
      ? [{ key: "sync-session", label: "同步会话", icon: icons.sessionImport }]
      : []),
    ...usageTargets.value,
    {
      key: "runaway",
      label: props.site.isRunaway ? "恢复存活" : "标记为跑路",
      icon: props.site.isRunaway ? icons.heartPulse : icons.flag,
      danger: !props.site.isRunaway,
      success: props.site.isRunaway,
    },
    ...(isUsage
      ? []
      : [
          {
            key: "delete",
            label: "删除",
            icon: icons.trash,
            danger: true,
            separatorBefore: true,
          } satisfies CardMenuEntry,
        ]),
  ];
  return entries;
});

function visibleMenuItems(): HTMLButtonElement[] {
  if (!menuPopup.value) return [];
  return Array.from(
    menuPopup.value.querySelectorAll<HTMLButtonElement>(".card-menu-item"),
  );
}

function openMenu() {
  menuOpen.value = true;
  nextTick(() => {
    (visibleMenuItems()[0] ?? menuPopup.value)?.focus({ preventScroll: true });
  });
}

function closeMenu(restoreFocus = false) {
  if (!menuOpen.value) return;
  menuOpen.value = false;
  if (restoreFocus) menuTrigger.value?.focus({ preventScroll: true });
}

function toggleMenu() {
  if (menuOpen.value) closeMenu(true);
  else openMenu();
}

function onMenuKeydown(event: KeyboardEvent) {
  const items = visibleMenuItems();
  if (items.length === 0) return;
  const current = items.indexOf(document.activeElement as HTMLButtonElement);
  switch (event.key) {
    case "ArrowDown": {
      event.preventDefault();
      items[(current + 1) % items.length].focus();
      break;
    }
    case "ArrowUp": {
      event.preventDefault();
      items[(current - 1 + items.length) % items.length].focus();
      break;
    }
    case "Home": {
      event.preventDefault();
      items[0]?.focus();
      break;
    }
    case "End": {
      event.preventDefault();
      items[items.length - 1]?.focus();
      break;
    }
    case "Escape": {
      event.preventDefault();
      closeMenu(true);
      break;
    }
  }
}

function runMenuAction(key: string, event: Event) {
  const trigger = event.currentTarget as HTMLElement;
  switch (key) {
    case "models":
      store.openSiteModelsDialog(props.site);
      break;
    case "preview":
      store.openPreview(props.site, trigger);
      break;
    case "edit":
      store.openModal(props.site);
      break;
    case "usage-personal":
      void store.setUsageState(props.site, "personal");
      break;
    case "usage-pending":
      void store.setUsageState(props.site, "pending");
      break;
    case "usage-unused":
      void store.setUsageState(props.site, "unused");
      break;
    case "sync-session":
      store.syncChromeSession(props.site, trigger);
      break;
    case "runaway":
      void store.toggleRunaway(props.site);
      break;
    case "delete":
      void store.deleteSite(props.site);
      break;
    default:
      break;
  }
  menuOpen.value = false;
}

function onDocumentPointerDown(event: PointerEvent) {
  if (!menuOpen.value) return;
  const target = event.target;
  if (target instanceof Node && menuRoot.value?.contains(target)) return;
  closeMenu();
}

function onDocumentKeydown(event: KeyboardEvent) {
  if (!menuOpen.value) return;
  if (event.key === "Escape") {
    event.preventDefault();
    closeMenu(true);
  }
}

onMounted(() => {
  document.addEventListener("pointerdown", onDocumentPointerDown, true);
  document.addEventListener("keydown", onDocumentKeydown, true);
});

onUnmounted(() => {
  document.removeEventListener("pointerdown", onDocumentPointerDown, true);
  document.removeEventListener("keydown", onDocumentKeydown, true);
});
</script>

<template>
  <article
    class="site-card"
    :class="{
      'is-runaway': site.isRunaway,
      'is-personal': site.isPersonal,
      'is-pending': site.isPending,
      'is-usage-mode': usageCardMode,
    }"
    :data-id="site.id"
  >
    <div class="card-top">
      <div class="site-avatar">{{ logo }}</div>
      <div class="site-main">
        <div class="title-row">
          <h2 :title="site.systemType ? `${site.name}（${systemTypeLabel(site.systemType)}）` : site.name">
            {{ site.name }}<span v-if="site.systemType" class="site-system-type">（{{ systemTypeLabel(site.systemType) }}）</span>
          </h2>
          <div class="card-actions">
            <div ref="menuRoot" class="card-menu">
              <button
                ref="menuTrigger"
                type="button"
                class="card-menu-trigger"
                :class="{ 'is-open': menuOpen }"
                title="更多操作"
                aria-label="更多操作"
                aria-haspopup="menu"
                :aria-expanded="menuOpen"
                :aria-controls="cardMenuId"
                @click.stop="toggleMenu"
                v-html="icons.more"
              />
              <div
                v-if="menuOpen"
                :id="cardMenuId"
                ref="menuPopup"
                class="card-menu-popup"
                role="menu"
                :aria-label="`${site.name} 的操作`"
                tabindex="-1"
                @keydown="onMenuKeydown"
              >
                <template v-for="entry in menuEntries" :key="entry.key">
                  <div v-if="entry.separatorBefore" class="card-menu-sep" role="separator" />
                  <button
                    type="button"
                    role="menuitem"
                    class="card-menu-item"
                    :class="{
                      'is-danger': entry.danger,
                      'is-success': entry.success,
                    }"
                    @click="runMenuAction(entry.key, $event)"
                  >
                    <span class="card-menu-item-icon" v-html="entry.icon" />
                    <span class="card-menu-item-label">{{ entry.label }}</span>
                  </button>
                </template>
              </div>
            </div>
          </div>
        </div>
        <time class="site-updated-at" :datetime="displayedUpdatedAt">
          更新时间 {{ formatDate(displayedUpdatedAt) }}
        </time>
        <div v-if="!usageCardMode" class="meta-chips">
          <span class="level-chip">LV{{ site.registrationLimit }}</span>
          <span v-if="site.requiresInviteCode" class="invite-chip">邀请码</span>
          <span v-if="rateLimit" class="rate-chip" :title="`速率限制：${rateLimit}`">{{ rateLimit }}</span>
        </div>
      </div>
    </div>

    <template v-if="!usageCardMode">
      <TagList :tags="site.tags" :is-personal="site.isPersonal" :is-pending="site.isPending" />

      <p class="description" :class="{ muted: !site.description }">
        {{ site.description || "暂无描述，稍后可以补充站点说明。" }}
      </p>
    </template>

    <div v-else class="usage-account-list">
      <div
        v-for="session in accountSessions"
        :key="session.profileId"
        class="usage-account-row"
      >
        <span class="usage-account-icon" v-html="icons.user" />
        <div class="usage-account-identity">
          <strong :title="session.username ? `${accountIdentity(session)}（${session.newapiUserId ? session.newapiUserId + ':' : ''}${session.username}）` : accountIdentity(session)">
            <span>{{ accountIdentity(session) }}</span>
            <span v-if="session.username" class="usage-account-username">（{{ session.newapiUserId ? session.newapiUserId + ':' : '' }}{{ session.username }}）</span>
          </strong>
          <small>
            <span
              v-if="normalizeSystemType(site.systemType) === 'newapi2'"
              class="usage-account-token"
              :class="{ 'has-token': session.hasAccessToken }"
              :title="session.hasAccessToken ? '此账号已缓存 NewAPI 访问令牌' : '此账号尚未取得 NewAPI 访问令牌'"
            >{{ session.hasAccessToken ? "有访问令牌" : "无访问令牌" }}</span>
            <span
              v-if="session.checkinEnabled || site.supportsCheckin || session.checkinError"
              class="usage-account-checkin"
              :class="{ 'is-checked': session.checkedInToday, 'has-error': session.checkinError, 'is-disabled': !session.checkedInToday && !session.checkinEnabled }"
              :title="session.checkinError || (session.checkedInToday ? '今日已签到' : (session.checkinEnabled ? '今日未签到' : '无法自动签到（404/403/未启用）'))"
            >{{ session.checkinError ? "签到异常" : (session.checkedInToday ? "已签到" : (session.checkinEnabled ? "未签到" : "无法签到")) }}</span>
            <span v-if="session.apiCountsSynced && !session.apiSyncError">
              {{ session.apiKeyCount ?? 0 }} 个 Key · {{ session.apiModelCount ?? 0 }} 个模型
            </span>
            <button
              v-else-if="session.apiSyncError"
              class="usage-account-api-action is-error"
              type="button"
              :title="`${session.apiSyncError}\n点击重新同步`"
              @click="store.syncChromeSession(site, $event.currentTarget as HTMLElement)"
            >Key 与模型同步失败，点击重试</button>
            <button
              v-else
              class="usage-account-api-action"
              type="button"
              title="点击同步 Key 与模型"
              @click="store.syncChromeSession(site, $event.currentTarget as HTMLElement)"
            >Key 与模型未同步，点击同步</button>
          </small>
        </div>
        <div class="usage-account-quota" :class="{ 'has-error': session.syncError }">
          <strong :title="session.syncError || `剩余额度：${accountQuota(session)}`">{{ accountQuota(session) }}</strong>
          <small :title="session.syncError">{{ session.syncError ? "账号信息同步失败" : "站点剩余额度" }}</small>
        </div>
      </div>
      <div v-if="accountSessions.length === 0" class="usage-account-empty">
        <span v-html="icons.user" />
        <p>{{ pendingMode ? "未检测到可展示的 Chrome 账户会话，请重新提取会话" : "此站点为手动在用标记，未检测到 Chrome 账户会话" }}</p>
      </div>
    </div>

    <div class="card-bottom">
      <div class="feature-actions">
        <button
          v-if="linkButtons.api"
          class="round-feature link-feature active"
          type="button"
          :aria-label="'API 地址'"
          :data-tooltip="linkButtons.api.tooltip"
          @click="store.openLinkDialog(site, 'api', $event.currentTarget as HTMLElement)"
          v-html="linkButtons.api.iconSvg"
        />
        <button
          v-if="linkButtons.checkin"
          class="round-feature link-feature active"
          type="button"
          :aria-label="'签到地址'"
          :data-tooltip="linkButtons.checkin.tooltip"
          @click="store.openLinkDialog(site, 'checkin', $event.currentTarget as HTMLElement)"
          v-html="linkButtons.checkin.iconSvg"
        />
        <button
          v-if="linkButtons.benefit"
          class="round-feature link-feature active"
          type="button"
          :aria-label="'福利站地址'"
          :data-tooltip="linkButtons.benefit.tooltip"
          @click="store.openLinkDialog(site, 'benefit', $event.currentTarget as HTMLElement)"
          v-html="linkButtons.benefit.iconSvg"
        />
        <button
          v-if="linkButtons.status"
          class="round-feature link-feature active"
          type="button"
          :aria-label="'状态页地址'"
          :data-tooltip="linkButtons.status.tooltip"
          @click="store.openLinkDialog(site, 'status', $event.currentTarget as HTMLElement)"
          v-html="linkButtons.status.iconSvg"
        />
        <button
          v-if="linkButtons.extension"
          class="round-feature link-feature active"
          type="button"
          :aria-label="'扩展链接'"
          :data-tooltip="linkButtons.extension.tooltip"
          @click="store.openLinkDialog(site, 'extension', $event.currentTarget as HTMLElement)"
          v-html="linkButtons.extension.iconSvg"
        />
      </div>
      <div class="capability-actions" v-if="capabilities.length">
        <button
          v-for="(cap, i) in capabilities"
          :key="i"
          class="round-feature active"
          :class="{ 'age-feature': cap.ageChip }"
          type="button"
          :title="cap.title"
        >
          <span v-if="cap.ageChip" class="age-label">18+</span>
          <span v-else v-html="cap.icon" />
        </button>
      </div>
    </div>

  </article>
</template>
