<script setup lang="ts" generic="T extends Record<string, any>">
import { computed, ref, watch } from "vue";
import { useTable } from "@tanstack/vue-table";
import {
  coreFeatures,
  rowPaginationFeature,
  rowSortingFeature,
  createCoreRowModel,
  createSortedRowModel,
  createPaginatedRowModel,
  type SortingState,
} from "@tanstack/table-core";
import { icons } from "../icons";

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
const features = { ...coreFeatures, rowPaginationFeature, rowSortingFeature };

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
  coreRowModel: createCoreRowModel() as any,
  sortedRowModel: createSortedRowModel() as any,
  paginatedRowModel: props.manualPagination ? undefined : (createPaginatedRowModel() as any),
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
const totalPages = computed(() => Math.max(1, Math.ceil(totalCount.value / props.pageSize)));
const currentPage = computed(() => Math.min(Math.max(1, props.page), totalPages.value));

const pageNumbers = computed<Array<number | "ellipsis">>(() => {
  const total = totalPages.value;
  const current = currentPage.value;
  if (total <= 7) return Array.from({ length: total }, (_, index) => index + 1);
  if (current <= 4) return [1, 2, 3, 4, 5, "ellipsis", total];
  if (current >= total - 3) return [1, "ellipsis", total - 4, total - 3, total - 2, total - 1, total];
  return [1, "ellipsis", current - 1, current, current + 1, "ellipsis", total];
});

function gotoPage(page: number) {
  emit("update:page", Math.max(1, Math.min(totalPages.value, page)));
}
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
void icons;
</script>

<template>
  <div class="app-table-wrap">
    <div class="app-table-scroll">
      <table class="app-table" role="table">
        <thead v-if="props.stickyHeader">
          <tr v-for="hg in headerGroups" :key="hg.id" role="row">
            <th
              v-for="header in hg.headers"
              :key="header.id"
              role="columnheader"
              :class="['app-table-th', props.columns[header.index]?.align && `align-${props.columns[header.index].align}`]"
              :style="props.columns[header.index]?.width ? { width: props.columns[header.index].width } : undefined"
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
              :style="col.width ? { width: col.width } : undefined"
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
      <label>每页
        <select :value="props.pageSize" @change="emit('update:pageSize', Number(($event.target as HTMLSelectElement).value))">
          <option v-for="opt in props.pageSizeOptions" :key="opt" :value="opt">{{ opt }}</option>
        </select>
        条
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
      <span class="app-table-page-total">{{ currentPage.toLocaleString() }}–{{ Math.min(currentPage * props.pageSize, totalCount).toLocaleString() }} / {{ totalCount.toLocaleString() }}</span>
    </footer>
  </div>
</template>
