<script setup lang="ts" generic="T extends Record<string, any>">
import { computed, ref, watch } from "vue";
import { useTable } from "@tanstack/vue-table";
import {
  coreFeatures,
  rowPaginationFeature,
  rowSortingFeature,
  tableFeatures,
  createSortedRowModel,
  createPaginatedRowModel,
  type SortingState,
} from "@tanstack/table-core";
import { icons } from "../../icons";
import CustomSelect from "./CustomSelect.vue";

export interface AppTableColumn {
  key: string;
  title: string;
  width?: string;
  align?: "left" | "right" | "center";
  sortable?: boolean;
  class?: string;
}

const props = withDefaults(
  defineProps<{
    rows: T[];
    columns: AppTableColumn[];
    rowKey?: (row: T) => string | number;
    loading?: boolean;
    emptyText?: string;
    page?: number;
    pageSize?: number;
    total?: number;
    manualPagination?: boolean;
    sorting?: SortingState;
    rowClass?: (row: T) => string;
    selectedKey?: string | number | null;
    clickable?: boolean;
    stickyHeader?: boolean;
    showPagination?: boolean;
    pageSizeOptions?: number[];
  }>(),
  {
    rowKey: (row: T) => (row.id ?? row.canonicalKey ?? "") as string | number,
    loading: false,
    emptyText: "没有数据",
    page: 1,
    pageSize: 50,
    total: 0,
    manualPagination: false,
    sorting: () => [] as SortingState,
    selectedKey: null,
    clickable: false,
    stickyHeader: true,
    showPagination: true,
    pageSizeOptions: () => [25, 50, 100],
  },
);

const emit = defineEmits<{
  (e: "update:page", value: number): void;
  (e: "update:pageSize", value: number): void;
  (e: "update:sorting", value: SortingState): void;
  (e: "row-click", row: T): void;
  (e: "select", row: T): void;
}>();

const internalSorting = ref<SortingState>(
  props.sorting?.length ? [...props.sorting] : [],
);
watch(
  () => props.sorting,
  (value) => {
    internalSorting.value = value?.length ? [...value] : [];
  },
);

const pageIndex = ref(Math.max(0, props.page - 1));
const pageSizeRef = ref(props.pageSize);
watch(() => props.page, (value) => { pageIndex.value = Math.max(0, value - 1); });
watch(() => props.pageSize, (value) => { pageSizeRef.value = value; });

// 每页条数：以内部状态为唯一事实源，保证选择器始终可用；
// 同时把当前值兜底并入可选项，避免父组件传入固定值（如 12/20）时下拉为空。
const pageSizeOptionsList = computed(() => {
  const set = new Set<number>(props.pageSizeOptions);
  set.add(pageSizeRef.value);
  return [...set].sort((a, b) => a - b);
});

// v9 的类型系统把 table 泛型收紧到 RowData，与组件泛型 T 不兼容。
// 组件内部按 any 使用 table 实例，对外暴露的 props/slots 仍保持 T 的强类型。
const columnDefs = computed(() =>
  props.columns.map((col) => ({
    id: col.key,
    accessorKey: col.key,
    header: col.title,
    enableSorting: col.sortable ?? true,
  })),
);

// v9 的 features 必须是对象（会被 spread 进 options），数组会导致 table 崩溃。
const features = tableFeatures({
  ...coreFeatures,
  rowPaginationFeature,
  rowSortingFeature,
  sortedRowModel: createSortedRowModel(),
  paginatedRowModel: createPaginatedRowModel(),
});

// state 必须传纯值对象；嵌套 ref 不会被自动解包，用 computed 保证每次读取都是值。
const tableState = computed(() => ({
  sorting: internalSorting.value,
  pagination: { pageIndex: pageIndex.value, pageSize: pageSizeRef.value },
}));

const table = useTable({
  features,
  columns: columnDefs as any,
  data: computed(() => props.rows) as any,
  state: tableState as any,
  onSortingChange: ((updater: SortingState | ((prev: SortingState) => SortingState)) => {
    internalSorting.value = typeof updater === "function" ? updater(internalSorting.value) : updater;
    emit("update:sorting", internalSorting.value);
  }) as any,
  onPaginationChange: ((updater: any) => {
    const current = { pageIndex: pageIndex.value, pageSize: pageSizeRef.value };
    const next = typeof updater === "function" ? updater(current) : updater;
    if (next.pageSize !== pageSizeRef.value) {
      pageSizeRef.value = next.pageSize;
      emit("update:pageSize", next.pageSize);
    }
    if (next.pageIndex !== pageIndex.value) {
      pageIndex.value = next.pageIndex;
      emit("update:page", next.pageIndex + 1);
    }
  }) as any,
  manualPagination: props.manualPagination,
  pageCount: props.manualPagination ? Math.max(1, Math.ceil(props.total / props.pageSize)) : undefined,
  rowCount: props.manualPagination ? props.total : undefined,
  autoResetPageIndex: false,
} as any);

const rows = computed(() => (table as any).getRowModel().rows as Array<{ original: T; id: string }>);
const headerGroups = computed(() => (table as any).getHeaderGroups() as Array<{
  id: string;
  headers: Array<{
    id: string;
    index: number;
    column: { id: string; getIsSorted: () => false | "asc" | "desc"; getCanSort: () => boolean; toggleSorting: (desc?: boolean) => void };
  }>;
}>);

const totalCount = computed(() => (props.manualPagination ? props.total : props.rows.length));
const totalPages = computed(() => Math.max(1, Math.ceil(totalCount.value / pageSizeRef.value)));
// 展示态统一跟随内部 pageIndex（实际渲染页），不能依赖可能未受控的 props.page：
// 未绑定 update:page 时父组件不回写，props.page 恒为初始值，表翻页但高亮/范围却不动。
const currentPage = computed(() => Math.min(Math.max(1, pageIndex.value + 1), totalPages.value));
const rangeStart = computed(() => totalCount.value === 0 ? 0 : (currentPage.value - 1) * pageSizeRef.value + 1);
const rangeEnd = computed(() => Math.min(currentPage.value * pageSizeRef.value, totalCount.value));

watch(totalPages, (pages) => {
  if (pageIndex.value + 1 > pages) {
    pageIndex.value = pages - 1;
    emit("update:page", pages);
  }
});

const pageNumbers = computed<Array<number | "ellipsis">>(() => {
  const total = totalPages.value;
  const current = currentPage.value;
  if (total <= 7) return Array.from({ length: total }, (_, index) => index + 1);
  if (current <= 4) return [1, 2, 3, 4, 5, "ellipsis", total];
  if (current >= total - 3) return [1, "ellipsis", total - 4, total - 3, total - 2, total - 1, total];
  return [1, "ellipsis", current - 1, current, current + 1, "ellipsis", total];
});

function gotoPage(page: number) {
  const nextPage = Math.max(1, Math.min(totalPages.value, page));
  pageIndex.value = nextPage - 1;
  emit("update:page", nextPage);
}
function changePageSize(value: number) {
  if (!Number.isFinite(value) || value <= 0) return;
  pageSizeRef.value = value;
  pageIndex.value = 0;
  emit("update:pageSize", value);
  emit("update:page", 1);
}
const pageSizeSelectOptions = computed(() =>
  pageSizeOptionsList.value.map((opt) => ({ value: opt, text: String(opt) })),
);
interface HeaderItem {
  id: string;
  index: number;
  column: { id: string; getIsSorted: () => false | "asc" | "desc"; getCanSort: () => boolean; toggleSorting: (desc?: boolean) => void };
}
function onSortClick(header: HeaderItem) {
  const sorted = header.column.getIsSorted();
  header.column.toggleSorting(sorted === "asc");
}
function onRowClick(row: T) {
  emit("row-click", row);
  emit("select", row);
}
function cellValue(row: T, key: string): unknown {
  return row[key];
}
// minmax(a, b) 是 grid 轨道专用值，直接用作 width 会被浏览器整条丢弃，
// 导致该列宽度失控（auto 布局下吞掉全部剩余空间或塌缩）。
// 转为 min-width 保留最小宽度，同时让该列在 auto 布局下自然吸收剩余空间。
function columnStyle(col: AppTableColumn): Record<string, string> | undefined {
  if (!col.width) return undefined;
  const match = col.width.match(/^minmax\(\s*([^,\s)]+)\s*,\s*[^)]*\)$/);
  if (match) return { minWidth: match[1].trim() };
  return { width: col.width };
}
void icons;
</script>

<template>
  <div class="app-table-wrap">
    <div class="app-table-scroll">
      <table class="app-table" role="table">
        <thead :class="{ 'is-sticky': props.stickyHeader }">
          <tr v-for="hg in headerGroups" :key="hg.id" role="row">
            <th
              v-for="header in hg.headers"
              :key="header.id"
              role="columnheader"
              :class="['app-table-th', props.columns[header.index]?.align && `align-${props.columns[header.index].align}`]"
              :style="columnStyle(props.columns[header.index])"
              :aria-sort="header.column.getIsSorted() === 'asc' ? 'ascending' : header.column.getIsSorted() === 'desc' ? 'descending' : undefined"
            >
              <button
                v-if="header.column.getCanSort()"
                type="button"
                class="app-table-th-btn"
                :class="{ sorted: header.column.getIsSorted() }"
                @click="onSortClick(header)"
              >
                <span>{{ props.columns[header.index]?.title }}</span>
                <i class="app-table-sort-ind" :class="{ asc: header.column.getIsSorted() === 'asc', desc: header.column.getIsSorted() === 'desc' }" v-html="icons.chevron" />
              </button>
              <span v-else>{{ props.columns[header.index]?.title }}</span>
            </th>
          </tr>
        </thead>
        <tbody>
          <tr
            v-for="row in rows"
            :key="props.rowKey(row.original)"
            role="row"
            :class="['app-table-tr', { clickable: props.clickable, active: props.selectedKey != null && props.rowKey(row.original) === props.selectedKey }, props.rowClass ? props.rowClass(row.original) : '']"
            :tabindex="props.clickable ? 0 : -1"
            @click="props.clickable && onRowClick(row.original)"
            @keydown.enter.prevent="props.clickable && onRowClick(row.original)"
            @keydown.space.prevent="props.clickable && onRowClick(row.original)"
          >
            <td
              v-for="col in props.columns"
              :key="col.key"
              role="cell"
              :class="['app-table-td', col.align && `align-${col.align}`, col.class]"
              :style="columnStyle(col)"
            >
              <slot :name="`cell-${col.key}`" :row="row.original" :value="cellValue(row.original, col.key)">{{ cellValue(row.original, col.key) ?? "—" }}</slot>
            </td>
          </tr>
        </tbody>
      </table>
      <div v-if="props.loading" class="app-table-empty">正在读取…</div>
      <div v-else-if="!rows.length" class="app-table-empty">{{ props.emptyText }}</div>
    </div>

    <footer v-if="props.showPagination && totalCount > 0" class="app-table-pagination">
      <label>
        <span>每页</span>
        <CustomSelect
          class="app-table-page-size"
          placement="top"
          :options="pageSizeSelectOptions"
          :model-value="pageSizeRef"
          aria-label="每页条数"
          @update:model-value="changePageSize(Number($event))"
        />
        <span>条</span>
      </label>
      <div class="app-table-page-buttons">
        <button type="button" :disabled="currentPage <= 1" @click="gotoPage(1)">首页</button>
        <button type="button" :disabled="currentPage <= 1" @click="gotoPage(currentPage - 1)">上一页</button>
        <button
          v-for="page in pageNumbers"
          :key="String(page)"
          type="button"
          :class="{ active: page === currentPage }"
          :disabled="page === 'ellipsis'"
          @click="typeof page === 'number' && gotoPage(page)"
        >{{ page === 'ellipsis' ? '…' : page }}</button>
        <button type="button" :disabled="currentPage >= totalPages" @click="gotoPage(currentPage + 1)">下一页</button>
        <button type="button" :disabled="currentPage >= totalPages" @click="gotoPage(totalPages)">末页</button>
      </div>
      <span class="app-table-page-total">{{ rangeStart.toLocaleString() }}–{{ rangeEnd.toLocaleString() }} / {{ totalCount.toLocaleString() }}</span>
    </footer>
  </div>
</template>
