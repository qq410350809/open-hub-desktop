<script setup lang="ts">
import { ref, reactive, watch, computed, nextTick } from "vue";
import { icons } from "../icons";
import { emptySite, type Maintainer, type ExtensionLink, type SiteRecord } from "../types";
import { useStore } from "../composables/useStore";
import CustomSelect from "./CustomSelect.vue";

const registrationLevelOptions = [
  { value: 0, text: "LV0" },
  { value: 1, text: "LV1" },
  { value: 2, text: "LV2" },
  { value: 3, text: "LV3" },
];

const store = useStore();

const activeTab = ref<"basic" | "features" | "maintenance">("basic");
const saving = ref(false);
const errorMessage = ref("");
const nameInputRef = ref<HTMLInputElement>();
const importUrlInputRef = ref<HTMLInputElement>();
const importUrl = ref("");

interface FormState {
  name: string;
  apiBaseUrl: string;
  description: string;
  registrationLimit: number;
  rateLimit: string;
  requiresInviteCode: boolean;
  tags: string;
  supportsCheckin: boolean;
  supportsImmersiveTranslation: boolean;
  supportsLdc: boolean;
  supportsNsfw: boolean;
  isPersonal: boolean;
  isPending: boolean;
  isRunaway: boolean;
  checkinUrl: string;
  benefitUrl: string;
  checkinNote: string;
  statusUrl: string;
  maintainers: Maintainer[];
  extensionLinks: ExtensionLink[];
}

const form = reactive<FormState>({
  name: "",
  apiBaseUrl: "",
  description: "",
  registrationLimit: 0,
  rateLimit: "",
  requiresInviteCode: false,
  tags: "",
  supportsCheckin: false,
  supportsImmersiveTranslation: false,
  supportsLdc: false,
  supportsNsfw: false,
  isPersonal: false,
  isPending: false,
  isRunaway: false,
  checkinUrl: "",
  benefitUrl: "",
  checkinNote: "",
  statusUrl: "",
  maintainers: [],
  extensionLinks: [],
});

function resetForm(site?: SiteRecord) {
  const value = site ?? emptySite();
  form.name = value.name;
  form.apiBaseUrl = value.apiBaseUrl;
  form.description = value.description;
  form.registrationLimit = value.registrationLimit;
  form.rateLimit = value.rateLimit;
  form.requiresInviteCode = value.requiresInviteCode;
  form.tags = value.tags.join(", ");
  form.supportsCheckin = value.supportsCheckin;
  form.supportsImmersiveTranslation = value.supportsImmersiveTranslation;
  form.supportsLdc = value.supportsLdc;
  form.supportsNsfw = value.supportsNsfw;
  form.isPersonal = value.isPersonal;
  form.isPending = value.isPending;
  form.isRunaway = value.isRunaway;
  form.checkinUrl = value.checkinUrl;
  form.benefitUrl = value.benefitUrl;
  form.checkinNote = value.checkinNote;
  form.statusUrl = value.statusUrl;
  form.maintainers = value.maintainers.length
    ? value.maintainers.map((m) => ({ ...m }))
    : [{ name: "", id: "", username: "", profileUrl: "" }];
  form.extensionLinks = value.extensionLinks.length
    ? value.extensionLinks.map((e) => ({ ...e }))
    : [{ label: "", url: "" }];
}

watch(
  () => store.modalOpen.value,
  (open) => {
    if (open) {
      resetForm(store.editingSite.value ?? undefined);
      importUrl.value = "";
      activeTab.value = "basic";
      errorMessage.value = "";
      nextTick(() => {
        if (store.editingId.value) nameInputRef.value?.focus();
        else importUrlInputRef.value?.focus();
      });
    }
  },
);

const selectedTags = computed(() =>
  form.tags
    .split(/[,，]/)
    .map((tag) => tag.trim())
    .filter(Boolean),
);

const suggestedTagsList = computed(() => store.suggestedTags.value);

function isTagSelected(tag: string): boolean {
  return selectedTags.value.includes(tag);
}

function toggleSuggestedTag(tag: string) {
  const tags = selectedTags.value;
  if (!tags.includes(tag)) tags.push(tag);
  else tags.splice(tags.indexOf(tag), 1);
  form.tags = tags.join(", ");
}

function addMaintainer() {
  form.maintainers.push({ name: "", id: "", username: "", profileUrl: "" });
}

function removeMaintainer(index: number) {
  form.maintainers.splice(index, 1);
}

function addExtension() {
  form.extensionLinks.push({ label: "", url: "" });
}

function removeExtension(index: number) {
  form.extensionLinks.splice(index, 1);
}

function setTab(tab: typeof activeTab.value) {
  activeTab.value = tab;
}

function closeModal() {
  store.closeModal();
}

async function handleSubmit() {
  errorMessage.value = "";
  if (!form.name) {
    errorMessage.value = "请输入站点名称";
    activeTab.value = "basic";
    return;
  }
  try {
    const url = new URL(form.apiBaseUrl);
    if (!["http:", "https:"].includes(url.protocol)) throw new Error();
  } catch {
    errorMessage.value = "请输入完整的 API BASE URL";
    activeTab.value = "basic";
    return;
  }

  const existing =
    store.editingId.value
      ? store.sites.value.find((site) => site.id === store.editingId.value) ?? emptySite()
      : emptySite();

  const maintainers = form.maintainers
    .map((item) => ({
      name: item.name.trim(),
      id: "",
      username: "",
      profileUrl: item.profileUrl.trim(),
    }))
    .filter((item) => item.name || item.profileUrl);

  const extensionLinks = form.extensionLinks
    .map((item) => ({
      label: item.label.trim(),
      url: item.url.trim(),
    }))
    .filter((item) => item.label || item.url);

  const input: SiteRecord = {
    ...existing,
    name: form.name.trim(),
    apiBaseUrl: form.apiBaseUrl.trim(),
    description: form.description.trim(),
    registrationLimit: Number(form.registrationLimit),
    rateLimit: form.rateLimit.trim(),
    requiresInviteCode: form.requiresInviteCode,
    tags: form.tags.split(/[,，]/).map((tag) => tag.trim()).filter(Boolean),
    supportsCheckin: form.supportsCheckin,
    supportsImmersiveTranslation: form.supportsImmersiveTranslation,
    supportsLdc: form.supportsLdc,
    supportsNsfw: form.supportsNsfw,
    isPersonal: form.isPersonal,
    isPending: form.isPending && !form.isPersonal,
    isRunaway: form.isRunaway,
    checkinUrl: form.checkinUrl.trim(),
    benefitUrl: form.benefitUrl.trim(),
    checkinNote: form.checkinNote.trim(),
    statusUrl: form.statusUrl.trim(),
    maintainers,
    extensionLinks,
  };

  saving.value = true;
  try {
    await store.saveSite(input);
  } finally {
    saving.value = false;
  }
}

async function handleImport() {
  errorMessage.value = "";
  try {
    const url = new URL(importUrl.value.trim());
    if (!["http:", "https:"].includes(url.protocol) || !url.hostname) throw new Error();
  } catch {
    errorMessage.value = "请输入完整的 http:// 或 https:// 站点 URL";
    return;
  }

  saving.value = true;
  try {
    await store.importSite(importUrl.value.trim());
  } catch (error) {
    errorMessage.value = String(error).replace(/^Error:\s*/, "");
  } finally {
    saving.value = false;
  }
}

function onBackdropClick(event: MouseEvent) {
  if (event.target === event.currentTarget) closeModal();
}
</script>

<template>
  <Teleport to="body">
    <div
      class="modal-backdrop"
      id="site-modal"
      :hidden="!store.modalOpen.value"
      @click="onBackdropClick"
    >
      <section
        class="site-modal"
        :class="{ 'is-import': !store.editingId.value }"
        role="dialog"
        aria-modal="true"
        aria-labelledby="modal-title"
      >
        <header class="modal-header">
          <div>
            <h2 id="modal-title">{{ store.editingId.value ? "编辑站点" : "新增站点" }}</h2>
            <p>{{ store.editingId.value ? "修改站点资料" : "导入站点" }}</p>
          </div>
          <button class="close-button" type="button" data-close-modal @click="closeModal" v-html="icons.close" />
        </header>

        <nav v-if="store.editingId.value" class="modal-tabs">
          <button
            class="tab-button"
            :class="{ active: activeTab === 'basic' }"
            type="button"
            data-tab="basic"
            @click="setTab('basic')"
          >
            <span v-html="icons.info" /><span>基础信息</span>
          </button>
          <button
            class="tab-button"
            :class="{ active: activeTab === 'features' }"
            type="button"
            data-tab="features"
            @click="setTab('features')"
          >
            <span v-html="icons.settings" /><span>功能配置</span>
          </button>
          <button
            class="tab-button"
            :class="{ active: activeTab === 'maintenance' }"
            type="button"
            data-tab="maintenance"
            @click="setTab('maintenance')"
          >
            <span v-html="icons.users" /><span>维护与扩展</span>
          </button>
        </nav>

        <form v-if="store.editingId.value" @submit.prevent="handleSubmit">
          <div class="modal-scroll">
            <!-- 基础信息 -->
            <section class="tab-panel" :class="{ active: activeTab === 'basic' }" data-panel="basic">
              <div class="form-grid two-cols">
                <label class="field">
                  <span>站点名称 <b>*</b></span>
                  <input
                    ref="nameInputRef"
                    v-model="form.name"
                    name="name"
                    required
                    maxlength="100"
                    placeholder="例如：My AI Service"
                  />
                </label>
                <label class="field">
                  <span>API BASE URL <b>*</b></span>
                  <input
                    v-model="form.apiBaseUrl"
                    name="apiBaseUrl"
                    type="url"
                    required
                    placeholder="https://api.example.com"
                  />
                </label>
                <label class="field field-wide">
                  <span>站点描述</span>
                  <textarea
                    v-model="form.description"
                    name="description"
                    rows="4"
                    maxlength="800"
                    placeholder="简要介绍站点的特色…"
                  />
                </label>
                <label class="field">
                  <span>等级限制（LV）</span>
                  <CustomSelect
                    :options="registrationLevelOptions"
                    :model-value="form.registrationLimit"
                    @update:model-value="val => form.registrationLimit = Number(val)"
                    aria-label="等级限制"
                  />
                  <small>等级限制范围为 0–3</small>
                </label>
                <label class="field">
                  <span>速率限制</span>
                  <input
                    v-model="form.rateLimit"
                    name="rateLimit"
                    placeholder="例如：10/min、500/20min、无限制"
                  />
                </label>
                <label class="check-card field-wide">
                  <input v-model="form.requiresInviteCode" name="requiresInviteCode" type="checkbox" />
                  <i></i>
                  <span>
                    <strong>注册时是否需要邀请码</strong>
                    <small>标记该站点注册时需要邀请码</small>
                  </span>
                </label>
                <label class="field field-wide">
                  <span>TAGS（支持的模型/功能）</span>
                  <input
                    v-model="form.tags"
                    name="tags"
                    id="tags-input"
                    placeholder="输入标签，用逗号分隔…"
                  />
                </label>
                <div class="suggested-tags field-wide">
                  <span>推荐标签（点击添加）：</span>
                  <div>
                    <button
                      v-for="tag in suggestedTagsList"
                      :key="tag"
                      class="suggest-tag"
                      :class="{ selected: isTagSelected(tag) }"
                      type="button"
                      :data-suggest-tag="tag"
                      @click="toggleSuggestedTag(tag)"
                    >
                      + {{ tag }}
                    </button>
                  </div>
                </div>
              </div>
            </section>

            <!-- 功能配置 -->
            <section class="tab-panel" :class="{ active: activeTab === 'features' }" data-panel="features">
              <h3 class="section-title">
                <span v-html="icons.settings" /> 功能开关
              </h3>
              <div class="feature-switches">
                <label class="check-card">
                  <input v-model="form.supportsCheckin" name="supportsCheckin" type="checkbox" />
                  <i></i>
                  <span><strong>支持签到</strong><small>是否支持每日签到</small></span>
                </label>
                <label class="check-card">
                  <input v-model="form.supportsImmersiveTranslation" name="supportsImmersiveTranslation" type="checkbox" />
                  <i></i>
                  <span><strong>支持沉浸式翻译</strong><small>是否可用于沉浸式翻译插件</small></span>
                </label>
                <label class="check-card">
                  <input v-model="form.supportsLdc" name="supportsLdc" type="checkbox" />
                  <i></i>
                  <span><strong>支持 LDC</strong><small>是否支持 Linux Do Credit</small></span>
                </label>
                <label class="check-card">
                  <input v-model="form.supportsNsfw" name="supportsNsfw" type="checkbox" />
                  <i></i>
                  <span><strong>支持 NSFW</strong><small>是否支持 NSFW</small></span>
                </label>
              </div>
              <h3 class="section-title section-spaced">
                <span v-html="icons.link" /> 相关链接
              </h3>
              <div class="form-grid two-cols">
                <label class="field">
                  <span>签到页 URL</span>
                  <input v-model="form.checkinUrl" name="checkinUrl" type="url" placeholder="默认 APIBaseUrl + /console/personal" />
                </label>
                <label class="field">
                  <span>福利站 URL</span>
                  <input v-model="form.benefitUrl" name="benefitUrl" type="url" placeholder="https://…" />
                </label>
                <label class="field">
                  <span>签到说明</span>
                  <input v-model="form.checkinNote" name="checkinNote" placeholder="例如：每日签到送 10 刀" />
                </label>
                <label class="field">
                  <span>状态页 URL</span>
                  <input v-model="form.statusUrl" name="statusUrl" type="url" placeholder="https://status…" />
                </label>
              </div>
            </section>

            <!-- 维护与扩展 -->
            <section class="tab-panel" :class="{ active: activeTab === 'maintenance' }" data-panel="maintenance">
              <h3 class="section-title">站点状态</h3>
              <label class="check-card">
                <input
                  v-model="form.isPersonal"
                  name="isPersonal"
                  type="checkbox"
                  @change="form.isPersonal && (form.isPending = false)"
                />
                <i></i>
                <span><strong>标记为在用</strong><small>我正在使用该站点</small></span>
              </label>
              <label class="check-card">
                <input
                  v-model="form.isPending"
                  name="isPending"
                  type="checkbox"
                  @change="form.isPending && (form.isPersonal = false)"
                />
                <i></i>
                <span><strong>标记为待定</strong><small>浏览器有会话，但尚未确认在用</small></span>
              </label>
              <label class="check-card runaway-check">
                <input v-model="form.isRunaway" name="isRunaway" type="checkbox" />
                <i></i>
                <span><strong>标记为已跑路</strong><small>保存后将站点归入"跑路"列表</small></span>
              </label>
              <div class="section-heading section-spaced">
                <h3>维护者信息</h3>
                <button class="secondary-button" id="add-maintainer" type="button" @click="addMaintainer">
                  <span v-html="icons.plus" /> 添加维护者
                </button>
              </div>
              <div class="dynamic-list">
                <div
                  v-for="(item, index) in form.maintainers"
                  :key="index"
                  class="dynamic-row maintainer-row"
                >
                  <label class="input-with-icon">
                    <span v-html="icons.link" />
                    <input
                      v-model="item.profileUrl"
                      data-maintainer-url
                      type="url"
                      placeholder="LD 个人主页：https://linux.do/u/xxx/summary"
                    />
                  </label>
                  <label class="input-with-icon">
                    <span v-html="icons.users" />
                    <input
                      v-model="item.name"
                      data-maintainer-name
                      placeholder="显示名称"
                    />
                  </label>
                  <button class="remove-row" type="button" title="删除" @click="removeMaintainer(index)">
                    <span v-html="icons.trash" />
                  </button>
                </div>
              </div>
              <div class="section-heading section-spaced">
                <h3>更多扩展链接</h3>
                <button class="secondary-button" id="add-extension" type="button" @click="addExtension">
                  <span v-html="icons.plus" /> 添加链接
                </button>
              </div>
              <div class="dynamic-list">
                <div
                  v-for="(item, index) in form.extensionLinks"
                  :key="index"
                  class="dynamic-row extension-row"
                >
                  <input v-model="item.label" data-extension-label placeholder="链接名称" />
                  <label class="input-with-icon">
                    <span v-html="icons.link" />
                    <input v-model="item.url" data-extension-url type="url" placeholder="https://…" />
                  </label>
                  <button class="remove-row" type="button" title="删除" @click="removeExtension(index)">
                    <span v-html="icons.trash" />
                  </button>
                </div>
              </div>
            </section>
          </div>

          <p class="form-error">{{ errorMessage }}</p>
          <footer class="modal-footer">
            <button class="secondary-button" type="button" data-close-modal @click="closeModal">取消</button>
            <button class="save-button" id="save-site" type="submit" :disabled="saving">
              {{ saving ? "正在保存…" : "保存" }}
            </button>
          </footer>
        </form>

        <form v-else class="site-import-form" @submit.prevent="handleImport">
          <div class="modal-scroll">
            <label class="field">
              <span>站点 URL <b>*</b></span>
              <div class="input-with-icon">
                <span v-html="icons.link" />
                <input
                  ref="importUrlInputRef"
                  v-model="importUrl"
                  name="siteUrl"
                  type="url"
                  required
                  inputmode="url"
                  autocomplete="url"
                  placeholder="https://example.com"
                  :disabled="saving"
                />
              </div>
            </label>
            <p v-if="saving" class="import-progress" role="status">正在采集站点资料…</p>
          </div>

          <p class="form-error">{{ errorMessage }}</p>
          <footer class="modal-footer">
            <button class="secondary-button" type="button" data-close-modal :disabled="saving" @click="closeModal">取消</button>
            <button class="save-button" id="import-site" type="submit" :disabled="saving || !importUrl.trim()">
              {{ saving ? "正在导入…" : "检测并导入" }}
            </button>
          </footer>
        </form>
      </section>
    </div>
  </Teleport>
</template>
