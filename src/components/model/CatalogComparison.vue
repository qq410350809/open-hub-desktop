<script setup lang="ts">
import { computed, ref } from "vue";
import type { ModelCatalogItem } from "../../types";
import { catalogComparisonRows } from "../../composables/model/catalogView";
import CatalogDialog from "./CatalogDialog.vue";

const props = defineProps<{ models: ModelCatalogItem[] }>();
const emit = defineEmits<{ close: []; remove: [id: string] }>();
const differencesOnly = ref(false);
const rows = computed(() => catalogComparisonRows(props.models)
  .filter((row) => !differencesOnly.value || row.differs));

function remove(id: string) {
  emit("remove", id);
  if (props.models.length <= 2) emit("close");
}
</script>

<template>
  <CatalogDialog title="模型参数对比" @close="emit('close')">
    <header class="catalog-compare-header">
      <p class="catalog-eyebrow">MODEL COMPARISON</p>
      <h2>模型参数对比</h2>
      <p class="catalog-muted">最多对比 4 个模型。价格单位为 USD / 百万 tokens，未声明的参数不作推断。</p>
      <label class="catalog-check">
        <input v-model="differencesOnly" type="checkbox" />
        只看不同项
      </label>
    </header>
    <div class="catalog-table-scroll catalog-comparison-scroll">
      <table class="catalog-comparison">
        <thead>
          <tr>
            <th scope="col">参数</th>
            <th v-for="model in models" :key="model.id" scope="col">
              <strong>{{ model.name }}</strong>
              <code class="catalog-secondary-id">{{ model.id }}</code>
              <button
                type="button"
                class="catalog-text-button"
                :aria-label="`从对比中移除 ${model.name}`"
                @click="remove(model.id)"
              >移除</button>
            </th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="row in rows" :key="row.label" :class="{ 'has-difference': row.differs }">
            <th scope="row">{{ row.label }}</th>
            <td v-for="(value, index) in row.values" :key="models[index].id">{{ value }}</td>
          </tr>
        </tbody>
      </table>
      <p v-if="!rows.length" class="catalog-empty-inline">这些模型在当前对比项中没有差异。</p>
    </div>
  </CatalogDialog>
</template>
