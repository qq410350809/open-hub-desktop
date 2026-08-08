// ECharts 按需注册（折线图 + 柱状图 + 热力图）
import * as echarts from "echarts/core";
import { LineChart, BarChart, HeatmapChart } from "echarts/charts";
import {
  GridComponent,
  TooltipComponent,
  LegendComponent,
  VisualMapComponent,
  DataZoomComponent,
} from "echarts/components";
import { CanvasRenderer } from "echarts/renderers";

echarts.use([
  LineChart,
  BarChart,
  HeatmapChart,
  GridComponent,
  TooltipComponent,
  LegendComponent,
  VisualMapComponent,
  DataZoomComponent,
  CanvasRenderer,
]);

export * from "echarts/core";
export type { EChartsOption } from "echarts";
