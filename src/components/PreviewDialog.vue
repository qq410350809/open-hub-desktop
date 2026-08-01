<script setup lang="ts">
import { computed, ref, watch, nextTick } from "vue";
import { icons } from "../icons";
import { formatDate, formatRateLimit, logoText } from "../utils";
import { useStore } from "../composables/useStore";
import type { AddressItem } from "../types";

const store = useStore();

const closeBtnRef = ref<HTMLButtonElement>();

const site = computed(() => store.previewSite.value);

const logo = computed(() =>
  site.value ? logoText(site.value.apiBaseUrl, site.value.name) : "",
);

const statusText = computed(() => {
  if (!site.value) return "";
  let text = site.value.isRunaway ? "已跑路" : "存活";
  if (site.value.isPersonal) text = `在用 (${text})`;
  return text;
});

const supportedFeatures = computed(() => {
  if (!site.value) return [];
  return [
    ["每日签到", site.value.supportsCheckin],
    ["沉浸式翻译", site.value.supportsImmersiveTranslation],
    ["LDC", site.value.supportsLdc],
    ["NSFW", site.value.supportsNsfw],
    ["邀请码", site.value.requiresInviteCode],
  ]
    .filter(([, supported]) => supported)
    .map(([label]) => String(label));
});

const previewAddressItems = computed<AddressItem[]>(() => {
  if (!site.value) return [];
  return store.allAddressItems(site.value);
});

const maintainers = computed(() => {
  if (!site.value) return [];
  return site.value.maintainers.map((item, index) => ({
    displayName: item.name || item.username || "未命名维护者",
    accountInfo: [item.username ? `@${item.username}` : "", item.id ? `ID：${item.id}` : ""]
      .filter(Boolean)
      .join(" · ") || "未配置账号信息",
    profileUrl: item.profileUrl,
    index,
  }));
});

function previewFact(value: string): string {
  return value || "未配置";
}

watch(
  () => store.previewDialogOpen.value,
  (open) => {
    if (open) {
      nextTick(() => closeBtnRef.value?.focus());
      document.body.classList.add("modal-open");
    } else {
      document.body.classList.remove("modal-open");
    }
  },
);

function close() {
  store.closePreview();
}

function onBackdropClick(event: MouseEvent) {
  if (event.target === event.currentTarget) close();
}

async function openAddress(item: AddressItem) {
  await store.openExternal(item.url);
}

async function copyAddress(item: AddressItem) {
  await store.copyAddress(item.url, item.label);
}

async function openProfile(url: string) {
  await store.openExternal(url);
}
</script>

<template>
  <Teleport to="body">
    <div
      class="preview-dialog-backdrop"
      id="preview-dialog"
      :hidden="!store.previewDialogOpen.value"
      @click="onBackdropClick"
    >
      <section
        class="preview-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby="preview-title"
      >
        <header class="preview-header">
          <div class="preview-heading">
            <div class="preview-avatar">{{ logo }}</div>
            <div>
              <p>站点详情</p>
              <h2 id="preview-title">{{ site?.name }}</h2>
              <div class="preview-heading-meta">
                <span :class="site?.isRunaway ? 'danger' : 'success'">{{ statusText }}</span>
                <span>LV{{ site?.registrationLimit }}</span>
              </div>
              <div v-if="supportedFeatures.length" class="preview-heading-features">
                <span
                  v-for="feature in supportedFeatures"
                  :key="feature"
                  class="preview-feature-tag"
                  :title="`支持${feature}`"
                >
                  <i></i>{{ feature }}
                </span>
              </div>
            </div>
          </div>
          <button
            ref="closeBtnRef"
            class="close-button"
            id="close-preview"
            type="button"
            aria-label="关闭站点预览"
            @click="close"
            v-html="icons.close"
          />
        </header>

        <div v-if="site" class="preview-scroll">
          <!-- 站点概览 -->
          <section class="preview-section preview-summary">
            <h3>站点概览</h3>
            <p>{{ site.description || "暂无站点描述" }}</p>
          </section>

          <!-- 基础信息 -->
          <section class="preview-section">
            <h3>基础信息</h3>
            <dl class="preview-facts">
              <div>
                <dt>API BASE URL</dt>
                <dd>{{ previewFact(site.apiBaseUrl) }}</dd>
              </div>
              <div>
                <dt>注册等级</dt>
                <dd>LV{{ site.registrationLimit }}</dd>
              </div>
              <div>
                <dt>速率限制</dt>
                <dd>{{ previewFact(formatRateLimit(site.rateLimit)) }}</dd>
              </div>
              <div>
                <dt>更新时间</dt>
                <dd>{{ formatDate(site.updatedAt) }}</dd>
              </div>
            </dl>
          </section>

          <!-- 标签 -->
          <section class="preview-section">
            <h3>标签</h3>
            <div class="preview-tags">
              <span v-if="!site.tags.length" class="muted">未配置标签</span>
              <span v-for="tag in site.tags" :key="tag">{{ tag }}</span>
            </div>
          </section>

          <!-- 相关链接 -->
          <section class="preview-section">
            <h3>相关链接 <span>{{ previewAddressItems.length }}</span></h3>
            <div class="preview-link-list">
              <template v-if="previewAddressItems.length">
                <div
                  v-for="(item, index) in previewAddressItems"
                  :key="index"
                  class="preview-link-row"
                >
                  <div>
                    <strong>{{ item.label }}</strong>
                    <small v-if="item.note?.trim()">{{ item.note.trim() }}</small>
                  </div>
                  <button
                    class="preview-link-value"
                    type="button"
                    :data-preview-open="index"
                    title="打开地址"
                    @click="openAddress(item)"
                  >{{ item.url }}</button>
                  <div class="preview-link-actions">
                    <button
                      type="button"
                      :data-preview-open="index"
                      title="打开地址"
                      @click="openAddress(item)"
                      v-html="icons.external"
                    />
                    <button
                      type="button"
                      :data-preview-copy="index"
                      title="复制地址"
                      @click="copyAddress(item)"
                      v-html="icons.copy"
                    />
                  </div>
                </div>
              </template>
              <p v-else class="preview-empty">未配置相关链接</p>
            </div>
          </section>

          <!-- 维护者 -->
          <section class="preview-section">
            <h3>维护者 <span>{{ site.maintainers.length }}</span></h3>
            <div class="preview-maintainers">
              <template v-if="maintainers.length">
                <div
                  v-for="m in maintainers"
                  :key="m.index"
                  class="preview-maintainer-row"
                >
                  <div>
                    <strong>{{ m.displayName }}</strong>
                    <small>{{ m.accountInfo }}</small>
                  </div>
                  <button
                    v-if="m.profileUrl"
                    type="button"
                    :data-preview-profile="m.index"
                    title="打开维护者主页"
                    @click="openProfile(m.profileUrl)"
                    v-html="icons.external"
                  />
                </div>
              </template>
              <p v-else class="preview-empty">未配置维护者信息</p>
            </div>
          </section>

          <!-- 本地状态 -->
          <section class="preview-section">
            <h3>本地状态</h3>
            <dl class="preview-facts">
              <div>
                <dt>运行状态</dt>
                <dd>{{ statusText }}</dd>
              </div>
              <div>
                <dt>在用状态</dt>
                <dd>{{ site.isPersonal ? "在用" : "未在用" }}</dd>
              </div>
              <div>
                <dt>信息可见范围</dt>
                <dd>{{ site.isOnlyMaintainerVisible ? "仅维护者可见" : "公开" }}</dd>
              </div>
              <div>
                <dt>公益属性</dt>
                <dd>{{ site.isFakeCharity ? "疑似伪公益" : "正常" }}</dd>
              </div>
              <div>
                <dt>待核实标记</dt>
                <dd>{{ site.hasPendingReport ? "有" : "无" }}</dd>
              </div>
            </dl>
          </section>
        </div>
      </section>
    </div>
  </Teleport>
</template>
