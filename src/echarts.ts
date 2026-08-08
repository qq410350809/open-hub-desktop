// ECharts 按需注册（折线图 + 热力图）
import * as echarts from "echarts/core";
import { LineChart, HeatmapChart } from "echarts/charts";
import {
  GridComponent,
  TooltipComponent,
  VisualMapComponent,
  DataZoomComponent,
} from "echarts/components";
import { CanvasRenderer } from "echarts/renderers";

echarts.use([
  LineChart,
  HeatmapChart,
  GridComponent,
  TooltipComponent,
  VisualMapComponent,
  DataZoomComponent,
  CanvasRenderer,
]);

export * from "echarts/core";
export type { EChartsOption } from "echarts";
