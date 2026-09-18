<script setup lang="ts">
import { computed, watch } from "vue";
import { icons } from "../../icons";
import { useStore } from "../../composables/useStore";

import CustomSelect from "./CustomSelect.vue";

/**
 * 站点地址的「浏览器账户」选择器（链接地址弹窗 / 站点预览弹窗共用）。
 *
 * 口径与站点卡片、列表完全一致（`store.siteSessions`）：卡片能列出几个账号，这里就提供
 * 几个可选项 —— 不按当前筛选（在用/待定/…）、也不按会话有效性过滤。多这两道过滤就会
 * 出现「卡片上明明有 2 个账号、弹窗却没有下拉框」。打开地址只需要 Chrome Profile，
 * 与令牌 / 额度是否有效无关（失效会话仍标注出来，但可选）。
 */
const props = defineProps<{
  /** 站点 id；为空（弹窗未打开）时不渲染 */
  siteId: string;
  /** 当前选中的 Chrome Profile id */
  modelValue: string;
}>();

const emit = defineEmits<{ "update:modelValue": [value: string] }>();

const store = useStore();

const sessions = computed(() => store.siteSessions(props.siteId));

const options = computed(() =>
  sessions.value.map((session) => ({
    value: session.profileId,
    text: store.sessionLabel(session),
  })),
);

// 默认选中第一个账号：单账号站点无需用户操作，多账号也保证不会因「空选」而回退默认浏览器
watch(
  () => [sessions.value.map((session) => session.profileId).join("|"), props.modelValue],
  () => {
    if (props.modelValue && sessions.value.some((s) => s.profileId === props.modelValue)) return;
    emit("update:modelValue", sessions.value[0]?.profileId ?? "");
  },
  { immediate: true },
);
</script>

<template>
  <div v-if="sessions.length" class="link-account-picker">
    <span class="link-account-icon" v-html="icons.user" />
    <label>
      <strong>浏览器账户</strong>
      <small>使用所选 Chrome Profile 打开地址</small>
    </label>
    <div class="link-account-select">
      <CustomSelect
        :options="options"
        :model-value="modelValue"
        aria-label="浏览器账户"
        @update:model-value="emit('update:modelValue', String($event))"
      />
    </div>
  </div>
</template>
