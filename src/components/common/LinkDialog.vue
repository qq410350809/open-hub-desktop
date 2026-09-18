<script setup lang="ts">
import { computed, ref, watch, nextTick } from "vue";
import { icons } from "../../icons";
import { useStore } from "../../composables/useStore";
import type { AddressItem, SiteLinkKind } from "../../types";

import LinkAccountPicker from "./LinkAccountPicker.vue";

const store = useStore();

const closeBtnRef = ref<HTMLButtonElement>();
const selectedProfileId = ref("");

const linkDialogTitles: Record<SiteLinkKind, string> = {
  api: "API 地址",
  checkin: "签到地址",
  benefit: "福利站地址",
  status: "状态页地址",
  extension: "扩展链接",
};

const visibleAddressItems = computed<AddressItem[]>(() => {
  if (!store.linkDialogSite.value) return [];
  return store.addressItems(store.linkDialogSite.value, store.linkDialogKind.value);
});

const subtitle = computed(() =>
  store.linkDialogSite.value
    ? `${store.linkDialogSite.value.name} · ${visibleAddressItems.value.length} 个地址`
    : "",
);

watch(
  () => store.linkDialogOpen.value,
  (open) => {
    if (open) {
      nextTick(() => closeBtnRef.value?.focus());
      document.body.classList.add("modal-open");
    } else {
      document.body.classList.remove("modal-open");
    }
  },
);

// 关闭弹窗时清掉已选账户，避免下次打开沿用到别的站点；
// 打开时由选择器默认选中第一个账号（单账号站点无需用户操作）
watch(
  () => [store.linkDialogOpen.value, store.linkDialogSite.value?.id],
  ([open]) => {
    if (!open) selectedProfileId.value = "";
  },
);

function close() {
  store.closeLinkDialog();
}

function onBackdropClick(event: MouseEvent) {
  if (event.target === event.currentTarget) close();
}

async function openAddress(item: AddressItem) {
  const site = store.linkDialogSite.value;
  if (!site) return;
  // 有关联账号 → 用所选（或第一个）账号的 Chrome Profile；没有任何账号 → 系统默认浏览器
  await store.openSiteAddress(item.url, site.id, selectedProfileId.value);
}

async function copyAddress(item: AddressItem) {
  await store.copyAddress(item.url, item.label);
}
</script>

<template>
  <Teleport to="body">
    <div
      class="link-dialog-backdrop"
      id="link-dialog"
      :hidden="!store.linkDialogOpen.value"
      @click="onBackdropClick"
    >
      <section
        class="link-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby="link-dialog-title"
      >
        <header class="link-dialog-header">
          <div>
            <h2 id="link-dialog-title">
              {{ store.linkDialogKind.value ? linkDialogTitles[store.linkDialogKind.value] : "" }}
            </h2>
            <p>{{ subtitle }}</p>
          </div>
          <button
            ref="closeBtnRef"
            class="close-button"
            id="close-link-dialog"
            type="button"
            aria-label="关闭地址列表"
            @click="close"
            v-html="icons.close"
          />
        </header>
        <LinkAccountPicker
          v-model="selectedProfileId"
          :site-id="store.linkDialogSite.value?.id ?? ''"
        />
        <div class="address-list">
          <div
            v-for="(item, index) in visibleAddressItems"
            :key="index"
            class="address-row"
          >
            <div class="address-details">
              <strong>{{ item.label }}</strong>
              <small v-if="item.note?.trim()">{{ item.note.trim() }}</small>
              <button
                class="address-value"
                type="button"
                :data-open-address="index"
                title="打开地址"
                @click="openAddress(item)"
              >{{ item.url }}</button>
            </div>
            <div class="address-actions">
              <button
                class="open-address"
                type="button"
                :data-open-address="index"
                :aria-label="`打开${item.label}`"
                title="打开地址"
                @click="openAddress(item)"
                v-html="icons.external"
              />
              <button
                class="copy-address"
                type="button"
                :data-copy-address="index"
                :aria-label="`复制${item.label}`"
                title="复制地址"
                @click="copyAddress(item)"
                v-html="icons.copy"
              />
            </div>
          </div>
        </div>
      </section>
    </div>
  </Teleport>
</template>
