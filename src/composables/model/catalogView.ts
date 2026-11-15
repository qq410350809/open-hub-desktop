import type { ModelCatalogHostItem, ModelCatalogItem, ModelCatalogProvider, ReasoningOption } from "../../types";
export const MODEL_KINDS = [
    { value: "all", label: "全部类型" }, { value: "text", label: "对话 / 文本" },
    { value: "image", label: "图像" }, { value: "video", label: "视频" },
    { value: "audio", label: "音频" }, { value: "embedding", label: "向量" },
    { value: "rerank", label: "重排" }, { value: "classify", label: "分类 / 审核" },
];
export const CAPABILITIES = [
    { key: "reasoning", label: "思考推理" }, { key: "toolCall", label: "工具调用" },
    { key: "structured", label: "结构化输出" }, { key: "attachment", label: "附件输入" },
    { key: "openWeights", label: "开放权重" }, { key: "temperature", label: "温度参数" },
    { key: "hasFastMode", label: "快速模式" },
] as const;
export type CapabilityKey = typeof CAPABILITIES[number]["key"];
export interface CatalogFilters {
    query: string;
    kind: string;
    lab: string;
    provider: string;
    status: string;
    pricing: "all" | "priced" | "free" | "subscription";
    origin: "all" | "official" | "thirdParty" | "unknown";
    minContext: number;
    capabilities: CapabilityKey[];
}
export type CatalogSort = "name" | "released" | "context" | "output" | "quality" | "speed" | "price";
export const CATALOG_SORTS: {
    value: CatalogSort;
    label: string;
}[] = [
    { value: "name", label: "名称 A–Z" }, { value: "released", label: "最新发布" },
    { value: "context", label: "上下文最多" }, { value: "output", label: "输出上限最多" },
    { value: "quality", label: "AA 质量最高" }, { value: "speed", label: "生成速度最快" },
    { value: "price", label: "输入价格最低" },
];
export function emptyCatalogFilters(): CatalogFilters {
    return { query: "", kind: "all", lab: "all", provider: "all", status: "all", pricing: "all", origin: "all", minContext: 0, capabilities: [] };
}
const LAB_NAMES: Record<string, string> = {
    openai: "OpenAI", anthropic: "Anthropic", google: "Google", deepseek: "DeepSeek",
    alibaba: "阿里 / Qwen", qwen: "阿里 / Qwen", zhipuai: "智谱 / GLM", "z-ai": "智谱 / GLM",
    meta: "Meta", mistral: "Mistral", "mistralai": "Mistral", xai: "xAI", moonshotai: "月之暗面 / Kimi",
    minimax: "MiniMax", microsoft: "Microsoft", nvidia: "NVIDIA", baidu: "百度", bytedance: "字节跳动",
    cohere: "Cohere", amazon: "Amazon", misc: "归属未确定", unknown: "归属未确定",
};
export function effectiveLab(model: ModelCatalogItem): string { return model.officialLab || model.lab || "misc"; }
export function labLabel(lab: string): string { return LAB_NAMES[lab.toLowerCase()] || lab || "归属未确定"; }
export function kindLabel(kind: string): string { return MODEL_KINDS.find((item) => item.value === (kind || "text"))?.label ?? kind; }
export function statusLabel(status: string): string {
    return ({ ga: "正式版", beta: "测试版", preview: "预览版", deprecated: "已废弃", alpha: "内测版" } as Record<string, string>)[status] || status || "未声明";
}
export function modalityLabel(value: string): string {
    return ({ text: "文本", image: "图像", audio: "音频", video: "视频", pdf: "PDF" } as Record<string, string>)[value] || value;
}
export function reasoningLevels(options?: ReasoningOption[]): string[] {
    return [...new Set((options ?? []).filter((option) => option.kind === "effort").flatMap((option) => option.values ?? []))];
}
export function reasoningText(model: Pick<ModelCatalogItem, "reasoning" | "reasoningOptions">): string {
    const levels = reasoningLevels(model.reasoningOptions);
    if (levels.length)
        return levels.join(" / ");
    if (model.reasoningOptions?.some((option) => option.kind === "toggle"))
        return "开 / 关";
    return model.reasoning ? "支持，档位未声明" : "未声明支持";
}
export function tokenText(value?: number | null): string {
    if (!value || !Number.isFinite(value) || value < 0)
        return "未声明";
    if (value >= 1000000)
        return `${Number((value / 1000000).toFixed(2))}M`;
    if (value >= 1000)
        return `${Number((value / 1000).toFixed(1))}K`;
    return value.toLocaleString();
}
export function tokenExactText(value?: number | null): string {
    return value != null && Number.isFinite(value) && value > 0
        ? `${value.toLocaleString("en-US")} tokens` : "未声明";
}
export function catalogComparisonRows(models: ModelCatalogItem[]) {
    return [
        { label: "厂商", values: models.map((model) => labLabel(effectiveLab(model))) },
        { label: "模型类型", values: models.map((model) => kindLabel(model.kind)) },
        { label: "上下文窗口", values: models.map((model) => tokenExactText(model.contextLength)) },
        { label: "最大输入", values: models.map((model) => tokenExactText(model.maxInputTokens)) },
        { label: "最大输出", values: models.map((model) => tokenExactText(model.maxOutputTokens)) },
        { label: "思考档位", values: models.map(reasoningText) },
        ...CAPABILITIES.map((capability) => ({ label: capability.label,
            values: models.map((model) => model[capability.key] ? "支持" : "未声明支持") })),
        { label: "官方输入 / 输出价格", values: models.map(referencePriceText) },
        { label: "免费渠道", values: models.map((model) => model.freeHostCount > 0 ? `${model.freeHostCount} 个` : "未登记") },
        { label: "公开渠道数", values: models.map((model) => String(model.hostCount)) },
        { label: "AA 综合质量", values: models.map((model) => model.aaIdx == null ? "未提供" : String(model.aaIdx)) },
        { label: "生成速度（tokens/s）", values: models.map((model) => model.aaSpeed == null ? "未提供" : String(model.aaSpeed)) },
        { label: "发布日期", values: models.map((model) => model.releaseDate || "未声明") },
        { label: "发布状态", values: models.map((model) => statusLabel(model.status)) },
    ].map((row) => ({ ...row, differs: new Set(row.values).size > 1 }));
}
export function priceText(value?: number | null): string {
    if (value == null || !Number.isFinite(value) || value < 0)
        return "未标价";
    return `$${Number(value.toFixed(6))}`;
}
export function hostPriceText(host: ModelCatalogHostItem, field: "input" | "output" | "cacheRead" | "cacheWrite"): string {
    if (host.subscription)
        return "订阅制";
    if (host.isFree && (field === "input" || field === "output"))
        return "免费";
    return priceText(host[field]);
}
export function modelInputPrice(model: ModelCatalogItem): number | null {
    if (model.freeHostCount > 0)
        return 0;
    return model.minInputCost > 0 ? model.minInputCost : null;
}
export function referencePriceText(model: ModelCatalogItem): string {
    if (!model.refOfficial)
        return model.officialHostCount > 0 ? "官方未标价" : "无官方渠道";
    if (model.refInputCost > 0 || model.refOutputCost > 0)
        return `${priceText(model.refInputCost)} / ${priceText(model.refOutputCost)}`;
    if (model.refProvider && model.freeChannelProviders?.includes(model.refProvider) && !model.subscriptionChannelProviders?.includes(model.refProvider))
        return "官方免费";
    return "官方未标价";
}
export interface CatalogIndexEntry {
    model: ModelCatalogItem;
    lab: string;
    search: string;
}
export function buildCatalogIndex(models: ModelCatalogItem[], providers: ModelCatalogProvider[]): CatalogIndexEntry[] {
    const names = new Map(providers.map((provider) => [provider.id, provider.name]));
    return models.map((model) => ({
        model, lab: effectiveLab(model),
        search: [model.id, model.name, model.slug, model.lab, model.officialLab, model.officialModelId,
            model.canonicalId, model.family, labLabel(effectiveLab(model)),
            ...(model.hostProviders ?? []).flatMap((id) => [id, names.get(id)])].filter(Boolean).join(" ").toLowerCase(),
    }));
}
function numberOrder(a: number | null | undefined, b: number | null | undefined, ascending = false): number {
    const valid = (value: number | null | undefined): value is number => value != null && Number.isFinite(value);
    if (!valid(a))
        return valid(b) ? 1 : 0;
    if (!valid(b))
        return -1;
    return ascending ? a - b : b - a;
}
export function filterCatalog(index: CatalogIndexEntry[], filters: CatalogFilters, sort: CatalogSort): ModelCatalogItem[] {
    const terms = filters.query.trim().toLowerCase().split(/\s+/).filter(Boolean);
    const list = index.filter(({ model, lab, search }) => {
        if (filters.kind !== "all" && (model.kind || "text") !== filters.kind)
            return false;
        if (filters.lab !== "all" && lab !== filters.lab)
            return false;
        if (filters.provider !== "all" && !model.hostProviders?.includes(filters.provider))
            return false;
        if (filters.status !== "all" && model.status !== filters.status)
            return false;
        if (filters.minContext > 0 && model.contextLength < filters.minContext)
            return false;
        if (filters.capabilities.some((capability) => !model[capability]))
            return false;
        if (filters.pricing === "free" && !(model.freeHostCount > 0))
            return false;
        if (filters.pricing === "subscription" && !(model.subHostCount > 0 || model.subscriptionChannelProviders?.length))
            return false;
        if (filters.pricing === "priced" && ![model.refInputCost, model.refOutputCost, model.minInputCost, model.minOutputCost].some((value) => value > 0))
            return false;
        if (filters.origin === "official" && !(model.officialHostCount > 0))
            return false;
        if (filters.origin === "thirdParty" && !(model.hostCount > model.officialHostCount))
            return false;
        if (filters.origin === "unknown" && model.identityResolved)
            return false;
        return terms.every((term) => search.includes(term));
    }).map((entry) => entry.model);
    return list.sort((a, b) => {
        let order = 0;
        const positive = (value: number) => value > 0 ? value : null;
        switch (sort) {
            case "released":
                order = numberOrder(a.releaseDate ? Date.parse(a.releaseDate) : null, b.releaseDate ? Date.parse(b.releaseDate) : null);
                break;
            case "context":
                order = numberOrder(positive(a.contextLength), positive(b.contextLength));
                break;
            case "output":
                order = numberOrder(positive(a.maxOutputTokens), positive(b.maxOutputTokens));
                break;
            case "quality":
                order = numberOrder(a.aaIdx, b.aaIdx);
                break;
            case "speed":
                order = numberOrder(a.aaSpeed, b.aaSpeed);
                break;
            case "price":
                order = numberOrder(modelInputPrice(a), modelInputPrice(b), true);
                break;
        }
        return order || a.name.localeCompare(b.name, "zh-CN", { numeric: true }) || a.id.localeCompare(b.id);
    });
}
export function catalogPage<T>(items: T[], page: number, pageSize: number) {
    const size = Math.max(1, Math.trunc(pageSize) || 25);
    const pages = Math.max(1, Math.ceil(items.length / size));
    const current = Math.max(1, Math.min(pages, Math.trunc(page) || 1));
    return { page: current, pages, start: items.length ? (current - 1) * size + 1 : 0,
        end: Math.min(current * size, items.length), items: items.slice((current - 1) * size, current * size) };
}
export function toggleCatalogComparison(ids: string[], id: string): string[] {
    return ids.includes(id) ? ids.filter((value) => value !== id) : ids.length < 4 ? [...ids, id] : ids;
}
export function estimateHostCost(host: ModelCatalogHostItem, inputMillions: number, outputMillions: number): number | null {
    if (host.subscription || !Number.isFinite(inputMillions) || !Number.isFinite(outputMillions) || inputMillions < 0 || outputMillions < 0)
        return null;
    if (host.isFree)
        return 0;
    if (inputMillions > 0 && (host.input == null || !Number.isFinite(host.input) || host.input < 0))
        return null;
    if (outputMillions > 0 && (host.output == null || !Number.isFinite(host.output) || host.output < 0))
        return null;
    return inputMillions * (host.input ?? 0) + outputMillions * (host.output ?? 0);
}
export function safeCatalogUrl(value: unknown): string | null {
    if (typeof value !== "string")
        return null;
    try {
        const url = new URL(value);
        return ["http:", "https:"].includes(url.protocol) && !url.username && !url.password ? url.href : null;
    }
    catch {
        return null;
    }
}
