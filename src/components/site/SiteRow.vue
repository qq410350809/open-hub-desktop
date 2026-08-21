<script setup lang="ts">
import { computed } from "vue";
import { icons } from "../../icons";
import { formatRateLimit, logoText } from "../../utils";
import { useStore } from "../../composables/useStore";
import TagList from "../common/TagList.vue";
import type { SiteRecord } from "../../types";

const props = defineProps<{
  site: SiteRecord;
}>();

const store = useStore();

const rateLimit = computed(() => formatRateLimit(props.site.rateLimit));
const logo = computed(() => logoText(props.site.apiBaseUrl, props.site.name));

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
</script>

<template>
  <article
    class="site-row"
    :class="{
      'is-runaway': site.isRunaway,
      'is-personal': site.isPersonal,
      'is-pending': site.isPending,
    }"
    :data-id="site.id"
  >
    <div class="site-row-avatar">
      <div class="site-avatar">{{ logo }}</div>
    </div>
    <span class="site-status-dot" title="本地记录" />

    <div class="site-row-content">
      <div class="site-row-identity">
        <h2 :title="site.name">{{ site.name }}</h2>
        <div class="meta-chips">
          <span class="level-chip">LV{{ site.registrationLimit }}</span>
          <span v-if="site.requiresInviteCode" class="invite-chip">邀请码</span>
          <span v-if="rateLimit" class="rate-chip" :title="`速率限制：${rateLimit}`">{{ rateLimit }}</span>
        </div>
      </div>
      <TagList :tags="site.tags" :is-personal="site.isPersonal" :is-pending="site.isPending" />
    </div>

    <div class="site-row-tools">
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
          type="button"
          :title="cap.title"
        >
          <span v-if="cap.ageChip" class="age-chip active" title="NSFW">18+</span>
          <span v-else v-html="cap.icon" />
        </button>
      </div>
    </div>

    <div class="site-row-actions card-actions">
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
        type="button"
        :data-edit="site.id"
        title="编辑"
        @click="store.openModal(site)"
        v-html="icons.edit"
      />
      <button
        class="usage-state-toggle"
        :class="{ 'is-personal': site.isPersonal, 'is-pending': site.isPending }"
        type="button"
        :data-usage-state="site.id"
        :title="site.isPersonal ? '当前：在用，点击切换为待定' : site.isPending ? '当前：待定，点击切换为未在用' : '当前：未在用，点击切换为在用'"
        :aria-label="site.isPersonal ? '当前：在用，点击切换为待定' : site.isPending ? '当前：待定，点击切换为未在用' : '当前：未在用，点击切换为在用'"
        @click="store.cycleUsageState(site)"
        v-html="site.isPending ? icons.clock : icons.bookmark"
      />
      <button
        class="runaway-toggle"
        :class="{ 'is-runaway': site.isRunaway }"
        type="button"
        :data-runaway="site.id"
        :title="site.isRunaway ? '恢复存活' : '标记为跑路'"
        :aria-label="site.isRunaway ? '恢复存活' : '标记为跑路'"
        @click="store.toggleRunaway(site)"
        v-html="site.isRunaway ? icons.heartPulse : icons.flag"
      />
    </div>
  </article>
</template>
