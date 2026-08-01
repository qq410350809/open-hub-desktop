<script setup lang="ts">
import { computed } from "vue";
import { icons } from "../icons";
import { formatRateLimit, logoText } from "../utils";
import { useStore } from "../composables/useStore";
import TagList from "./TagList.vue";
import type { ChromeSessionInfo, SiteRecord } from "../types";

const props = defineProps<{
  site: SiteRecord;
}>();

const store = useStore();

const rateLimit = computed(() => formatRateLimit(props.site.rateLimit));
const logo = computed(() => logoText(props.site.apiBaseUrl, props.site.name));
const personalMode = computed(() => store.usageFilter.value === "personal");
const accountSessions = computed(() =>
  (store.chromeUsageAccounts.value[props.site.id] ?? []).filter((session) => session.isValid),
);

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
</script>

<template>
  <article
    class="site-card"
    :class="{
      'is-runaway': site.isRunaway,
      'is-personal': site.isPersonal,
      'is-usage-mode': personalMode,
    }"
    :data-id="site.id"
  >
    <div class="card-top">
      <div class="site-avatar">{{ logo }}</div>
      <div class="site-main">
        <div class="title-row">
          <h2 :title="site.systemType ? `${site.name}（${site.systemType}）` : site.name">
            {{ site.name }}<span v-if="site.systemType" class="site-system-type">（{{ site.systemType }}）</span>
          </h2>
          <div class="card-actions">
            <button
              type="button"
              class="site-models-toggle"
              :data-models="site.id"
              title="查看支持的模型"
              aria-label="查看此站点的支持模型"
              @click="store.openSiteModelsDialog(site)"
              v-html="icons.cpu"
            />
            <button
              type="button"
              :data-preview="site.id"
              title="查看详情"
              aria-label="查看站点详情"
              @click="store.openPreview(site, $event.currentTarget as HTMLElement)"
              v-html="icons.info"
            />
            <button
              v-if="!personalMode"
              type="button"
              :data-edit="site.id"
              title="编辑"
              @click="store.openModal(site)"
              v-html="icons.edit"
            />
            <button
              class="personal-toggle"
              :class="{ active: site.isPersonal }"
              type="button"
              :data-personal="site.id"
              :title="site.isPersonal ? '取消在用' : '标记为在用'"
              :aria-label="site.isPersonal ? '取消在用' : '标记为在用'"
              @click="store.togglePersonal(site)"
              v-html="icons.bookmark"
            />
            <button
              v-if="personalMode"
              class="sync-session-toggle"
              type="button"
              :data-sync-session="site.id"
              title="同步会话"
              aria-label="同步会话"
              @click="store.syncChromeSession(site, $event.currentTarget as HTMLElement)"
              v-html="icons.restore"
            />
            <button
              v-if="!personalMode"
              class="runaway-toggle"
              :class="{ 'is-runaway': site.isRunaway }"
              type="button"
              :data-runaway="site.id"
              :title="site.isRunaway ? '恢复存活' : '标记为跑路'"
              :aria-label="site.isRunaway ? '恢复存活' : '标记为跑路'"
              @click="store.toggleRunaway(site)"
              v-html="site.isRunaway ? icons.restore : icons.flag"
            />
            <button
              v-if="!personalMode"
              class="delete-toggle"
              type="button"
              :data-delete="site.id"
              title="删除"
              @click="store.deleteSite(site)"
              v-html="icons.trash"
            />
          </div>
        </div>
        <div v-if="!personalMode" class="meta-chips">
          <span class="level-chip">LV{{ site.registrationLimit }}</span>
          <span v-if="site.requiresInviteCode" class="invite-chip">邀请码</span>
          <span v-if="rateLimit" class="rate-chip" :title="`速率限制：${rateLimit}`">{{ rateLimit }}</span>
        </div>
      </div>
    </div>

    <template v-if="!personalMode">
      <TagList :tags="site.tags" :is-personal="site.isPersonal" />

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
          <strong :title="session.username ? `${accountIdentity(session)}（${session.username}）` : accountIdentity(session)">
            <span>{{ accountIdentity(session) }}</span>
            <span v-if="session.username" class="usage-account-username">（{{ session.username }}）</span>
            <span
              v-if="session.checkinEnabled || session.checkinError"
              class="usage-account-checkin"
              :class="{ 'is-checked': session.checkedInToday, 'has-error': session.checkinError }"
              :title="session.checkinError || (session.checkedInToday ? '今日已签到' : '今日未签到')"
            >{{ session.checkinError ? "签到异常" : (session.checkedInToday ? "已签到" : "未签到") }}</span>
          </strong>
          <small>邮箱（站点用户名）</small>
        </div>
        <div class="usage-account-quota" :class="{ 'has-error': session.syncError }">
          <strong :title="session.syncError || `剩余额度：${accountQuota(session)}`">{{ accountQuota(session) }}</strong>
          <small :title="session.syncError">{{ session.syncError ? "账号信息同步失败" : "站点剩余额度" }}</small>
        </div>
      </div>
      <div v-if="accountSessions.length === 0" class="usage-account-empty">
        <span v-html="icons.user" />
        <p>此站点为手动在用标记，未检测到 Chrome 账户会话</p>
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
