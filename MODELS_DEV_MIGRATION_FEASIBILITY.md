# models.dev 替换模型全景控制台数据源 —— 可行性深度分析

> 分析对象：`https://models.dev/catalog.json`（4.95 MB，HTTP 200）
> 分析目标：评估用它替换 OpenHub「模型全景控制台」当前数据源的可行性
> 分析时间：2026-09-14

---

## 一、结论先行

**不建议直接替换，但值得作为「基础元数据源」引入。**

决定性理由：**你当前的数据源 llmpricing.dev 本身就是 models.dev 的下游聚合层。** 把数据源换成 models.dev，本质是"往上游退一层"——会丢掉 llmpricing 在上游基础上叠加的两个增强维度（ArtificialAnalysis 评测分、OpenRouter 用量），而控制台的 UI 恰好重度依赖 AA 评测分。

| 判断 | 结论 |
|---|---|
| 能否直接替换？ | ❌ 不能。会丢失 AA 评测与用量数据，导致 6 项 UI 功能直接失效 |
| 能否作为基础源引入？ | ✅ 可以。基础元数据字段质量更高，且新增 license / weights / links 等字段 |
| 推荐做法 | 双源并存：models.dev 供基础元数据 + llmpricing（或直连 AA/OpenRouter）供评测与用量 |
| 工作量评估 | 中等。核心成本在重写「跨渠道价格聚合」与「canonical 归并」两套算法 |

---

## 二、关键发现：数据源血缘

这是本次分析最重要的一条，它直接决定了可行性判断的走向。

### 2.1 血缘证据

llmpricing.dev 的 manifest 元数据中明确标注了上游来源：

```json
{
  "meta": {
    "syncedAt": "2026-09-13T07:08:33.782Z",
    "dataUpdatedAt": "2026-09-13",
    "providers": 213,
    "models": 1941,
    "providerModels": 7780,
    "source": "https://models.dev",
    "aa":  { "version": "4.1", "attached": 238, "source": "https://artificialanalysis.ai/" },
    "openrouter": { "asOf": "2026-09-13T07:08:07.162Z", "attached": 75, "source": "https://openrouter.ai/rankings" }
  }
}
```

### 2.2 三重交叉验证

| 验证项 | models.dev | llmpricing.dev | 结果 |
|---|---|---|---|
| 供应商 ID 集合 | 213 个 | 213 个 | **完全一致，零差异** |
| 渠道模型记录数 | 7785 条 | 7780 条 | 差 5 条（过滤规则差异） |
| 价格数据逐条比对 | `glm-5.2` → 1.4 / 4.4 / 0.26 | `zhipuai/glm-5.2` ref → 1.4 / 4.4 / 0.26 | **完全相同** |

### 2.3 依赖链路

```
models.dev ──────┐
（基础元数据）    │
                 ├──→ llmpricing.dev ──→ OpenHub 模型全景控制台
artificialanalysis.ai ─┤   （聚合层）        （ModelCatalogItem · 46 字段）
（AA 评测分）     │        ↑ 当前数据源
                 │
openrouter.ai ───┘
（用量排行）
```

**结论**：models.dev 覆盖的是「基础元数据层」，llmpricing 在其上叠加了「评测」和「用量」两个增强层。当前控制台消费的是**叠加后的结果**。

---

## 三、规模与结构对比

### 3.1 数据规模

| 维度 | models.dev `catalog.json` | models.dev `api.json` | llmpricing.dev |
|---|---|---|---|
| 文件体积 | 4.95 MB（单文件） | 4.64 MB（单文件） | 49 KB + 5 分片 ≈ 1.62 MB |
| 顶层结构 | `{models, providers}` | `{provider → {models}}` | `{meta, providers, total, shards}` + 分片数组 |
| 供应商数 | 213 | 213 | 213 |
| canonical 模型数 | 395（上游策展） | — | 1941（自行归并） |
| 渠道模型记录 | 7785 | 7785 | 7780 |
| 价格字段覆盖率 | 7345 / 7785（94%） | 94% | 94% |
| 鉴权 | 无 | 无 | 无 |
| 缓存策略 | `cf-cache-status: HIT`，`max-age=0, must-revalidate` | 同左 | — |

### 3.2 两个可选格式

models.dev 提供两个端点，用途不同：

- **`catalog.json`**（4.95 MB）：`models` 是上游策展的 395 条 canonical 模型，`providers` 是 213 家供应商及其渠道模型。canonical 层**不含价格**，价格在 `providers[].models[].cost`。
- **`api.json`**（4.64 MB）：AI SDK 标准格式，`provider → models`，**每条渠道模型内联 `cost`**。结构与当前 llmpricing 分片最接近，迁移成本更低。

> 若决定接入，**优先选 `api.json`**——它的形态与现有 `model_catalog_models` 表结构天然对齐。

### 3.3 canonical 覆盖差异

- models.dev canonical（395）与 llmpricing（1941）重叠 **382 条（97%）**
- llmpricing 独有 **1559 条**（长尾模型，如 `abliteration-ai/*`、`aion-labs/*`）
- models.dev 独有 **13 条**（多为即将下线的旧版本）

**含义**：models.dev 的 canonical 列表只覆盖头部 395 款模型。若要复现 llmpricing 的 1941 条全量视图，**必须自己实现「渠道模型 → canonical 归并」逻辑**——这正是 llmpricing 已经在替你做的事，也是迁移的主要工作量之一。

---

## 四、字段级映射分析（46 个字段）

对照 `src/types.ts` 的 `ModelCatalogItem` 接口与 `src-tauri/src/model/catalog/catalog.rs` 的解析逻辑，逐字段核对。

### 4.1 汇总

| 分类 | 数量 | 占比 |
|---|---|---|
| ✅ 可直接映射 | 15 | 33% |
| ⚠️ 需本地聚合计算 | 23 | 50% |
| ❌ 数据源完全缺失 | 8 | 17% |

### 4.2 完整映射表

#### ✅ 可直接映射（15 项）

| UI 字段 | models.dev 路径 | 覆盖率 |
|---|---|---|
| `id` | `models[].id` / `providers[].models[].id` | 100% |
| `name` | `.name` | 100% |
| `family` | `.family` | 96% |
| `knowledge` | `.knowledge` | 57% |
| `openWeights` | `.open_weights` | 100% |
| `reasoning` | `.reasoning` | 100% |
| `toolCall` | `.tool_call` | 100% |
| `attachment` | `.attachment` | 100% |
| `structured` | `.structured_output` | 41% |
| `temperature` | `.temperature` | 97% |
| `inputModalities` | `.modalities.input` | 100% |
| `contextLength` | `.limit.context` | 100% |
| `maxOutputTokens` | `.limit.output` | 100% |
| `releaseDate` | `.release_date` | 100% |
| `lastUpdated` | `.last_updated` | 100% |

> 这 15 项不仅可替代，**数据质量还更高**：models.dev 额外提供 `description`（llmpricing 行里没有）、`license`（44/395）、`weights`（150/395）、`links`（18/395）、`reasoning_options`、`interleaved`。

#### ⚠️ 需本地聚合计算（23 项）

| UI 字段 | 计算方式 | 可行性 |
|---|---|---|
| `slug` | 由 id/name 生成 | ✅ 易 |
| `lab` | 从 canonical id 前缀（`openai/xxx`）推导 | ✅ 易 |
| `contextMin` / `contextMax` | 聚合各渠道 `limit.context` 的极值 | ✅ 易 |
| `refProvider` / `refOfficial` | 官方供应商识别规则 | ✅ 中 |
| `refInputCost` / `refOutputCost` / `refCacheReadCost` | 取官方渠道 `cost` | ✅ 中 |
| `minProvider` / `minInputCost` / `minOutputCost` / `minCacheReadCost` | 跨渠道 `cost` 求最小 | ✅ 中 |
| `priceSpread` | max / min 价格比 | ✅ 易 |
| `blendedMin` / `blendedTrusted` / `blendedRef` | 加权混合价算法（需复刻 llmpricing 口径） | ⚠️ 难 |
| `hostCount` / `pricedHostCount` / `freeHostCount` / `subHostCount` / `hostProviders` | 反查 `providers[].models` 聚合 | ✅ 中 |
| `benchmarkCount` | `.benchmarks` 数组长度（结构与 AA 不同） | ✅ 中 |

> `blendedTrusted` 等混合价指标目前无公开口径定义，**只能自行设计算法**，会导致迁移前后数值不可比。

#### ❌ 数据源完全缺失（8 项）

| UI 字段 | 来源 | 影响 |
|---|---|---|
| `kind` | llmpricing 自建（text/image/video/audio/embedding/classify/rerank） | 分类筛选失效 |
| `status` | llmpricing 自建（ga/beta） | 只能默认 `ga` |
| `aaIdx` | artificialanalysis.ai | **6 项 UI 失效** |
| `aaCoding` | artificialanalysis.ai | 详情页仪表盘失效 |
| `aaAgentic` | artificialanalysis.ai | 详情页仪表盘失效 |
| `aaSpeed` | artificialanalysis.ai | 筛选 / 排序 / 指标卡失效 |
| `aaTtft` | artificialanalysis.ai | 详情页指标失效 |
| `aaTaskCost` | artificialanalysis.ai | 详情页指标失效 |

**关于 `kind` 的专项验证**：尝试用 `modalities` 推导 `kind` 已被证明不可靠。实测数据：

| llmpricing kind | 最常见的 inputModalities | 说明 |
|---|---|---|
| `image` (69) | `["text"]` × 43 | 多数生图模型输入只有文本，无法与文本模型区分 |
| `video` (67) | `["text"]` × 29 | 同上 |
| `audio` (60) | `["text"]` × 30 | 同上 |
| `embedding` (48) | `["text"]` × 44 | 同上 |
| `rerank` (7) | `["text"]` × 6 | 同上 |

结论：**`kind` 无法从 models.dev 可靠推导**，只能靠模型名关键词匹配，准确率存疑。

### 4.3 当前 UI 未消费但可用的字段

- llmpricing 行中的 `usage{tokens, rank, share}`（OpenRouter 用量，75/1941 = 3% 覆盖）——`ModelCatalogItem` 中无对应字段，当前未消费。
- llmpricing 行中的 `aa.variant`（评测变体，如 `xhigh` / `max`）——同样未消费。

---

## 五、UI 影响面

对照 `src/components/pages/ModelCatalogPage.vue`（4034 行）逐项核对。

### 5.1 完全失效（6 项）

| 位置 | 功能 | 代码依据 |
|---|---|---|
| 表格列 | 「AA 质量 / 速度」列 | `tableColumns` 中 `aaScores` |
| 筛选器 | 「高质量」`aaIdx >= 80` | L239 |
| 筛选器 | 「高速度」`aaSpeed >= 100` | L238 |
| 排序 | 「评测分」「速度」排序 | L258 / L260 |
| 指标卡 | 「最高评测分」「最快模型」 | L136 / L139 / L689 |
| 详情页 | 4 个 AA 仪表盘（idx / coding / speed / ttft） | L1559–1580 |

### 5.2 需重写或降级（7 项）

| 位置 | 功能 | 说明 |
|---|---|---|
| 筛选器 | `kind` 分类 | 无法可靠推导 |
| 筛选器 | `status` | 只能默认 `ga` |
| 表格列 | 参考价格 / 全网最低价 | 需重写跨渠道聚合 |
| 卡片 / 详情 | 省钱徽章 `priceSpread` | 需重写 |
| 详情页 | 价格指数（blended × 3） | 口径不明，需自建 |
| 表格列 | 支持渠道数 `hostCount` | 需反查聚合 |
| 表格列 | benchmarkCount | 结构转换 |

### 5.3 原样保留（15 项）

名称、标识、厂商、上下文容量、最大输出、推理/工具调用/结构化/温度标志、模态、开源权重、附件支持、发布/更新日期——全部保留，**且新增 license / weights / links 可供展示**。

### 5.4 影响量化

- 表格 8 列中，**6 列可替代，1 列（AA）失效，1 列（价格）需重写**
- 6 个筛选器中，**2 个直接失效**（高质量 / 高速度）
- 5 个排序项中，**2 个失效**（评测分 / 速度）
- 4 个指标卡中，**2 个失效**（最高评测分 / 最快模型）

---

## 六、三种落地方案对比

### 方案 A：完全替换为 models.dev

直接改 `catalog.rs` 的 `LLMPRICING_MANIFEST_URL` 指向 models.dev，重写解析层。

- ✅ 数据源更权威、更上游，字段更丰富（license / weights / links / description）
- ✅ 单文件 4.95 MB，无需分片拼接，逻辑更简单
- ❌ **丢失 AA 评测与用量数据，6 项 UI 功能直接失效**
- ❌ 需自行实现 canonical 归并（复现 1941 条视图）
- ❌ 需自行实现跨渠道价格聚合（23 个字段）
- ❌ `blendedTrusted` 等指标口径不可复现，数值前后不可比
- **适用**：愿意砍掉 AA 相关功能，只做纯元数据目录

### 方案 B：双源并存（推荐）

models.dev 供基础元数据，llmpricing（或直连 artificialanalysis + openrouter）供评测与用量。

- ✅ 保留全部 46 个字段，UI 零改动
- ✅ 基础元数据质量提升，新增 license / weights / links
- ✅ 降低对单一第三方聚合站（llmpricing）的依赖风险
- ⚠️ 需处理两源 ID 对齐（canonical id 与 provider-model id 的映射）
- ⚠️ 同步逻辑变复杂：两个数据源、两套缓存与失败降级
- **适用**：想提升数据质量又不想损失功能

### 方案 C：维持现状 + 补充校验

继续用 llmpricing 作主源，把 models.dev 作为**校验基准**（比对价格与元数据一致性，发现异常告警）。

- ✅ 零风险，零 UI 改动
- ✅ 能发现 llmpricing 的数据漂移或聚合错误
- ❌ 不解决对单一聚合站的依赖
- **适用**：保守策略，先观察

### 对比表

| 维度 | 方案 A 完全替换 | 方案 B 双源并存 | 方案 C 校验基准 |
|---|---|---|---|
| UI 改动量 | 大（删 6 项功能） | 零 | 零 |
| 后端改动量 | 大 | 中 | 小 |
| 字段完整性 | 38 / 46 | 46 / 46 | 46 / 46 |
| 数据质量 | 中（丢评测） | 高 | 中 |
| 依赖风险 | 低 | 低 | 高 |
| 推荐度 | ⭐⭐ | ⭐⭐⭐⭐⭐ | ⭐⭐⭐ |

---

## 七、推荐方案实施要点（方案 B）

### 7.1 数据源分工

| 层 | 数据源 | 提供字段 |
|---|---|---|
| 基础元数据 | `models.dev/api.json` | id, name, description, family, knowledge, modalities, limit, 能力标志, license, weights, links, cost |
| 评测 | `artificialanalysis.ai` | aaIdx, aaCoding, aaAgentic, aaSpeed, aaTtft, aaTaskCost |
| 用量 | `openrouter.ai/rankings` | usage（可选，当前未消费） |
| 派生计算 | 本地 | slug, lab, kind, status, 价格聚合, blended 系列, 渠道统计 |

### 7.2 需要新增/重写的模块

1. **models.dev 抓取器**：`api.json` 单文件拉取，替代分片逻辑（`catalog.rs` L1511–1542 的分片循环可简化）
2. **canonical 归并器**：`provider → canonical` 映射规则（新模块）
3. **价格聚合器**：跨渠道 `cost` 求 ref / min / spread（复用现有逻辑，改输入源）
4. **blended 计算器**：需明确口径定义（**迁移前必须先冻结算法**）
5. **ID 对齐表**：models.dev canonical id ↔ llmpricing id 映射（可用 id 前缀 + 归一化匹配，重叠率 97%）
6. **降级策略**：AA 源失败时保留上次缓存，避免 UI 出现空值

### 7.3 注意事项

- **`schema_version` 需递增**：`CATALOG_SCHEMA_VERSION` 当前为 `9`，改数据源后应递增以触发重建
- **`model_catalog_sources` 表**：当前硬编码 `'llmpricing_manifest'`（L395 / L709 / L952），需扩展为多源
- **接口稳定性**：models.dev 为社区项目（SST 开源），`catalog.json` 无正式 schema 文档、无稳定性承诺；响应头 `cache-control: max-age=0, must-revalidate` 表明内容每次校验，但**无版本化 API**，字段变更无预警
- **文件体积**：单文件 4.95 MB，全量拉取比当前分片模式（1.62 MB）更耗流量；建议加 ETag 条件请求（响应头已提供 `etag`）

---

## 八、风险清单

| 风险 | 等级 | 说明 | 缓解 |
|---|---|---|---|
| AA 数据丢失 | 🔴 高 | 6 项 UI 直接失效，且无替代源 | 保留 llmpricing 或直连 artificialanalysis |
| `blended*` 口径不可复现 | 🔴 高 | 迁移前后数值不可比，用户会察觉 | 迁移前冻结算法口径 |
| `kind` 无法推导 | 🟠 中 | 273 个非文本模型无法分类 | 保留 llmpricing 的 kind，或建人工映射表 |
| canonical 归并偏差 | 🟠 中 | 归并规则不同会导致模型数量变化 | 以 llmpricing 结果做回归测试 |
| 上游接口无版本化 | 🟠 中 | models.dev 字段可能静默变更 | 加 schema 校验 + 失败告警 |
| 单点依赖 | 🟡 低 | 双源后风险下降 | 双源互为备份 |

---

## 九、一句话总结

**models.dev 是你当前数据源的上游，不是替代品。** 它的基础元数据质量更高（多出 license / weights / links / description），值得作为基础层引入；但它完全没有 AA 评测分和 OpenRouter 用量数据，而这两项支撑着控制台 6 项核心 UI 功能。推荐采用**双源并存**方案：models.dev 供元数据，artificialanalysis 供评测，本地做归并与价格聚合。

---

## 附录：本次分析的数据依据

| 数据 | 来源 | 体积 |
|---|---|---|
| models.dev catalog.json | `https://models.dev/catalog.json` | 4,954,702 B |
| models.dev api.json | `https://models.dev/api.json` | 4,641,330 B |
| llmpricing manifest | `https://llmpricing.dev/rows/manifest.json` | 49,042 B |
| llmpricing 分片 × 5 | `https://llmpricing.dev/rows/rows-00{0..4}.json` | ≈ 1.62 MB |

代码依据：
- `src/types.ts` L908–1018（`ModelCatalogItem` 等接口定义）
- `src-tauri/src/model/catalog/catalog.rs` L13–14（数据源常量）、L300–355（表结构）、L404–534（解析逻辑）、L1511–1542（同步流程）
- `src/components/pages/ModelCatalogPage.vue` L301–310（表格列）、L136–278（指标/筛选/排序）、L1559–1580（详情页仪表盘）
