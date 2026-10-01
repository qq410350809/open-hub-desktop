import type { LocalToolConfigSnapshot, ModelCapabilities } from "../../types";

export const THINKING_EFFORT_OPTIONS = ["off", "minimal", "low", "medium", "high", "xhigh", "max"] as const;
export type ParamSource = "override" | "parentUser" | "catalog" | "hydrated" | "snapshot" | "none";
export const SOURCE_LABELS: Record<ParamSource, string> = {
  override: "自定义", parentUser: "父级自定义", catalog: "目录",
  hydrated: "配置文件", snapshot: "Agent 默认", none: "未设",
};
export interface ModelParamOverride {
  contextWindow?: number | null;
  maxOutput?: number | null;
  defaultReasoningEffort?: string;
  efforts?: string[];
}

export function normalizeEfforts(values: string[]): string[] {
  const levels = values.map((v) => v.trim().toLowerCase()).map((v) => v === "none" ? "off" : v);
  return THINKING_EFFORT_OPTIONS.filter((level) => levels.includes(level));
}

/** null = 未声明档位，[] = 已知没有可供 effort 选择的档位；不能把后者扩成全集。 */
export function catalogEfforts(caps: ModelCapabilities | null | undefined): string[] | null {
  if (!caps) return null;
  const options = caps.reasoningOptions ?? [];
  const declared = options.filter((o) => o.kind === "effort");
  if (declared.length) return normalizeEfforts(declared.flatMap((o) => o.values));
  if (!caps.supportsReasoning || options.some((o) => o.kind === "toggle")) return [];
  return null;
}

/**
 * 目录声明的默认思考级别 = **该模型最高可配置档位**。
 *
 * 取自声明档位集合的最高档；集合缺失时退回后端算好的 `reasoningEffortMax`。
 * 无声明（未命中 / 仅 toggle / 不支持思考）返回 null，不臆造。
 */
export function catalogDefaultEffort(caps: ModelCapabilities | null | undefined): string | null {
  const declared = catalogEfforts(caps);
  if (declared && declared.length) {
    return THINKING_EFFORT_OPTIONS.filter((level) => declared.includes(level)).at(-1) ?? null;
  }
  const max = caps?.reasoningEffortMax?.trim().toLowerCase();
  if (max) {
    const normalized = normalizeEfforts([max]);
    if (normalized.length) return normalized[normalized.length - 1];
  }
  return null;
}

export function catalogParams(caps: ModelCapabilities | null | undefined): ModelParamOverride {
  if (!caps) return {};
  const efforts = catalogEfforts(caps);
  const declared = catalogDefaultEffort(caps);
  return {
    ...(caps.contextLength > 0 ? { contextWindow: caps.contextLength } : {}),
    ...(caps.maxOutputTokens > 0 ? { maxOutput: caps.maxOutputTokens } : {}),
    ...(efforts !== null ? { efforts } : {}),
    ...(declared ? { defaultReasoningEffort: declared } : {}),
  };
}

export function snapshotParams(snap: LocalToolConfigSnapshot | null): ModelParamOverride {
  return {
    contextWindow: snap?.context.contextWindow ?? null,
    maxOutput: snap?.context.maxOutputTokens ?? null,
    defaultReasoningEffort: snap?.defaults.reasoningEffort || snap?.thinking.effortLevel || "",
    efforts: normalizeEfforts(snap?.defaults.reasoningEffortOptions?.length
      ? snap.defaults.reasoningEffortOptions
      : snap?.thinking.effortLevelOptions?.length ? snap.thinking.effortLevelOptions : [...THINKING_EFFORT_OPTIONS]),
  };
}

/** 仅保存用户改过的字段；undefined 表示恢复默认，null/0/[]/"" 都是显式值。 */
export function patchModelParams(current: ModelParamOverride, patch: ModelParamOverride): ModelParamOverride {
  const next = { ...current, ...patch };
  for (const key of Object.keys(next) as (keyof ModelParamOverride)[]) {
    if (next[key] === undefined) delete next[key];
  }
  return next;
}

export function modelParamGroupKey(modelMode: boolean, channelId: string, account: string, model: string): string {
  return modelMode ? `model::${model.slice(model.lastIndexOf("/") + 1).toLowerCase()}` : `${channelId}::${account}`;
}

/** 不按值相等推断用户意图；未改字段始终继续跟随下一层。 */
export function resolveModelParams(
  child: ModelParamOverride = {}, parent: ModelParamOverride = {},
  caps: ModelCapabilities | null = null, disk: ModelParamOverride = {}, fallback: ModelParamOverride = {},
) {
  const catalog = catalogParams(caps);
  const layers: [ParamSource, ModelParamOverride][] = [
    ["override", child], ["parentUser", parent], ["catalog", catalog],
    ["hydrated", disk], ["snapshot", fallback],
  ];
  function pick<K extends keyof ModelParamOverride>(key: K, empty: NonNullable<ModelParamOverride[K]> | null) {
    const found = layers.find(([, value]) => value[key] !== undefined);
    return { value: found ? found[1][key] : empty, source: found?.[0] ?? "none" as ParamSource };
  }
  const context = pick("contextWindow", null);
  const output = pick("maxOutput", null);
  const effort = pick("defaultReasoningEffort", "");
  // 思考级别筛选是「勾了哪些档」，不是模型能力本身：父级显式筛选优先于目录声明全集。
  // 目录仍约束可选范围（声明为空 = 没有可供 effort 选择的档），未筛选时才回退目录 / 配置文件。
  const efforts = child.efforts !== undefined
    ? { value: child.efforts, source: "override" as ParamSource }
    : parent.efforts !== undefined
      ? {
          value: catalog.efforts !== undefined
            ? parent.efforts.filter((level) => catalog.efforts!.includes(level))
            : parent.efforts,
          source: "parentUser" as ParamSource,
        }
      : pick("efforts", []);
  const hasContextWindowOverride = child.contextWindow !== undefined;
  const hasMaxOutputOverride = child.maxOutput !== undefined;
  const hasEffortOverride = child.defaultReasoningEffort !== undefined;
  const hasEffortsListOverride = child.efforts !== undefined;
  const overriddenCount = [hasContextWindowOverride, hasMaxOutputOverride, hasEffortOverride, hasEffortsListOverride].filter(Boolean).length;
  return {
    contextWindow: context.value as number | null,
    maxOutput: output.value as number | null,
    defaultReasoningEffort: (effort.value ?? "") as string,
    efforts: (efforts.value ?? []) as string[],
    contextWindowSource: context.source, maxOutputSource: output.source,
    defaultReasoningEffortSource: effort.source, effortsSource: efforts.source,
    hasContextWindowOverride, hasMaxOutputOverride, hasEffortOverride, hasEffortsListOverride,
    isOverridden: overriddenCount > 0, overriddenCount,
  };
}

/** 每次回读都重建磁盘层；绝不修改 preferences 的自定义层，即使两者数值相等。 */
export function hydrateModelParams(snap: LocalToolConfigSnapshot | null): Record<string, ModelParamOverride> {
  const out: Record<string, ModelParamOverride> = {};
  if (!snap) return out;
  for (const model of snap.models) {
    out[`${model.provider}::${model.id}`] = {
      ...(model.contextWindow > 0 ? { contextWindow: model.contextWindow } : {}),
      ...(model.maxOutput > 0 ? { maxOutput: model.maxOutput } : {}),
    };
  }
  for (const [key, raw] of Object.entries(snap.defaults.perModelEffort ?? {})) {
    const at = key.indexOf("/");
    if (at < 1) continue; // Claude 的 opus/sonnet/haiku 是模型映射，不是 effort。
    const provider = key.slice(0, at);
    const model = snap.tool === "opencode" ? key : key.slice(at + 1);
    const target = `${provider}::${model}`;
    out[target] = { ...out[target], efforts: normalizeEfforts(raw.split(",")) };
  }
  return out;
}
