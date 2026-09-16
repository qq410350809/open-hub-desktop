# 以 models.dev 为主源、llmpricing 为辅源 —— 架构方案与实施计划

> 承接《MODELS_DEV_MIGRATION_FEASIBILITY.md》
> 本文回答：能否把 models.dev 扶正为主源，llmpricing 降为辅源？如何落地？
> 分析时间：2026-09-14

---

## 一、结论

**可行，且推荐。** 但有一个必须避开的坑。

数据验证结果比预期好得多：

| 验证项 | 结果 | 判定 |
|---|---|---|
| 两源 ID 命名空间 | 同一套 `lab/model` 格式 | ✅ 可直接 join |
| ID 精确匹配率 | 382 / 395 = **96.7%** | ✅ 无需模糊匹配 |
| `ref` 参考价格一致性 | 301 / 302 = **99.7%** | ✅ models.dev 可完整复现 |
| 基础元数据一致性 | name 仅 6/382 有格式差异 | ✅ 高度一致 |

**唯一需要警惕的是：绝不能用 models.dev 的 canonical 列表当模型清单。**

---

## 二、必须先避开的坑

models.dev `catalog.json` 的 `models` 字典只有 **395 条**（上游策展的头部模型），而当前控制台有 **1941 款**。如果按字面理解"以 models.dev 为主"、直接用这 395 条当清单：

```
395 款  ← 只剩这些
1941 款 ← 当前
丢失 1559 款（80%）
```

**正确做法是取并集：**

| 方案 | 模型数 | 说明 |
|---|---|---|
| 仅 models.dev canonical | 395 | ❌ 丢 1559 款 |
| 仅 llmpricing | 1941 | 现状 |
| **并集** | **1954** | ✅ 净增 13 款 |

并集后的来源分布：

| 分组 | 数量 | 元数据来源 |
|---|---|---|
| 双源覆盖 | 382 | models.dev 权威 + llmpricing 增强 |
| 仅 llmpricing | 1559 | 维持现状（长尾模型） |
| 仅 models.dev | 13 | 纯新增 |

> 13 款新增包含 `anthropic/claude-3-5-sonnet-20241022`、`amazon/nova-premier`、`swiss-ai/apertus-8b`、`deepreinforce/ornith-*` 等。

---

## 三、关键验证细节

### 3.1 ID 对齐：96.7% 精确匹配，无需模糊匹配

```
models.dev canonical :  openai/gpt-5.5,  anthropic/claude-sonnet-4-5,  alibaba/qwen-max
llmpricing           :  zhipuai/glm-5.2, openai/gpt-5.6-terra-pro
```

两者是**同一套命名空间**。归一化（小写 / 去点号）后**没有任何额外命中**，说明格式已高度统一。

未命中的 13 款多为即将下线的旧版本（`claude-3-5-sonnet-20241022`、`command-r-08-2024`、`claude-opus-4-0`）和未收录的新模型（`veo-3.1-*`、`minimax/image-01`）。

**⚠️ 命名空间陷阱**：models.dev 的 **provider 级** ID 是另一套格式，**不能与 canonical ID 混用**：

| 层级 | ID 格式 | 示例 |
|---|---|---|
| canonical `models` | `lab/model` | `openai/gpt-5.5` |
| provider `providers[].models` | `provider/model` | `subconscious/glm-5.2`、`@cf/deepseek-ai/...` |

要用 provider 级数据，需按 **bare id（末段）** 跨全部 213 家供应商搜索。实测该策略定位率 **356/382 = 93%**。

### 3.2 价格：ref 完全可复现，min 不可

这是本次验证最有价值的发现。

**`ref` 参考价格 —— 一致率 99.7%（301/302）**

llmpricing 的 `ref` 就是 models.dev 同一渠道的 `cost`。之前看到的"25 条价格冲突"是**比对口径错误**造成的假象：

```
qwen-max
  models.dev  alibaba 渠道     → 1.6 / 6.4    （国际站，USD）
  models.dev  alibaba-cn 渠道  → 0.345 / 1.377（国内站）
  llmpricing  ref              → 0.345 / 1.377 @ alibaba-cn  ✅ 取自国内站
```

**数据完全一致，只是 models.dev 把国际站和国内站拆成了两个 provider。** 唯一真实差异是 `deepseek/deepseek-v4-pro-0813 @ edenai`（md 0.5808 vs lp 2.0）。

**`min` 全网最低价 —— 一致率仅 59.7%（197/330）**

llmpricing 的 `min` **不是简单的全渠道最小值**，它有一套过滤规则（排除免费渠道、可信渠道白名单）：

| 模型 | models.dev 原始最低 | llmpricing min | 差异原因 |
|---|---|---|---|
| `alibaba/qwen3-32b` | 0（ovhcloud 免费） | 0.09 | 排除免费渠道 |
| `alibaba/qwen2.5-coder-32b-instruct` | 0（huggingface 免费） | 0.06 | 排除免费渠道 |
| `alibaba/qwen3-235b-a22b` | 0.287 | 0.3 | 渠道白名单 |

**结论**：`ref` 可以改用 models.dev 自行计算；**`min` 建议继续用 llmpricing**，除非愿意复刻它的过滤规则。

### 3.3 context / outputLimit：语义差异，不是冲突

70/382 的 `context` 值不同，82/378 的 `outputLimit` 不同。但这不是数据错误，是**两个字段语义不同**：

```
anthropic/claude-sonnet-4-5
  models.dev  limit.context = 200,000    ← 官方规格声明值
  llmpricing  context       = 1,000,000  ← 跨 20 家渠道的代表值
  llmpricing  contextRange  = [200000, 1000000]  ← 渠道范围
```

**设计建议**：两者**并存而非互相覆盖**——
- `contextLength` ← models.dev（官方规格，用户预期值）
- `contextMin` / `contextMax` ← llmpricing `contextRange`（跨渠道能力）

这反而让 UI 能表达"官方 200K / 渠道最高 1M"这种更有价值的信息。

### 3.4 意外收获：models.dev 比预期更全

models.dev 的 **provider 层**藏着 canonical 层没有的字段：

| 字段 | provider 层覆盖 | 说明 |
|---|---|---|
| `cost` | 7345 / 7785（94%） | 价格原料 |
| `reasoning_options` | 5593 / 7785 | 对 382 匹配模型覆盖 **65%** |
| `interleaved` | 1027 / 7785 | 对 382 匹配模型覆盖 **27%** |
| `status` | 264 条（deprecated 195 / beta 69） | 稀疏标注，缺省即 ga |
| `experimental` | 38 条 | |
| `description` | 7785 / 7785（100%） | |

**这意味着原先判定为"缺失"的 `status` 其实可以替代**——语义是"只标异常状态，无标注默认 ga"，与 llmpricing 的 `ga/deprecated/beta` 等价。

### 3.5 真正无法替代的字段：只剩 7 个

| 字段 | 来源 | 影响 |
|---|---|---|
| `kind`（8 类分类） | llmpricing 自建 | 分类筛选失效 |
| `aaIdx` / `aaCoding` / `aaAgentic` / `aaSpeed` / `aaTtft` / `aaTaskCost` | artificialanalysis.ai | 6 项 UI 失效 |

`kind` 无法从 modalities 推导（已验证：image 类 69 个中 43 个输入模态只有 `["text"]`）。

### 3.6 models.dev 为 382 匹配模型净新增的字段

| 字段 | 覆盖 | 价值 |
|---|---|---|
| `description` | 382 / 382（100%） | 模型简介，UI 可直接展示 |
| `reasoning_options` | 250 / 382（65%） | 推理档位配置 |
| `benchmarks` | 130 / 382（34%） | 评测明细（含来源链接） |
| `weights` | 144 / 382（37%） | HuggingFace 权重链接 |
| `interleaved` | 105 / 382（27%） | 推理字段名 |
| `license` | 38 / 382（9%） | 开源协议 |
| `links` | 12 / 382（3%） | 论文/主页链接 |

---

## 四、字段归属矩阵

| 字段 | 主源 models.dev | 辅源 llmpricing | 本地派生 |
|---|---|---|---|
| `id` `name` `family` `knowledge` | ✅ 权威 | | |
| `openWeights` `reasoning` `toolCall` `attachment` `structured` `temperature` | ✅ 权威 | | |
| `inputModalities` `contextLength` `maxOutputTokens` | ✅ 权威 | | |
| `releaseDate` `lastUpdated` | ✅ 权威 | | |
| `status` | ✅ provider 层标注 | 兜底 | 缺省 `ga` |
| `description` `reasoning_options` `interleaved` `weights` `license` `links` `benchmarks` | ✅ 新增 | | |
| `refInputCost` `refOutputCost` `refCacheReadCost` `refProvider` `refOfficial` | ✅ 原料 cost | | ✅ 聚合计算 |
| `slug` `lab` | | | ✅ 推导生成 |
| `kind` | | ✅ 唯一来源 | |
| `aaIdx` `aaCoding` `aaAgentic` `aaSpeed` `aaTtft` `aaTaskCost` | | ✅ 唯一来源 | |
| `minInputCost` `minOutputCost` `minCacheReadCost` `minProvider` | | ✅ 含过滤规则 | |
| `blendedMin` `blendedTrusted` `blendedRef` | | ✅ 唯一来源 | |
| `contextMin` `contextMax` | | ✅ `contextRange` | |
| `hostCount` `pricedHostCount` `freeHostCount` `subHostCount` `hostProviders` | | ✅ 唯一来源 | |
| `priceSpread` | | ✅ | |
| `benchmarkCount` | ✅ 可计算 | 兜底 | |
| `usage` | | ✅ 唯一来源 | |

**统计**：主源负责 **23 项**，辅源负责 **17 项**，本地派生 **4 项**，两源兜底 **2 项**。

---

## 五、数据流架构

```
┌─────────────────────────────────────────────┐
│ models.dev/catalog.json  (4.95 MB 单文件)   │
│  ├─ models{}        → 395 canonical 元数据  │
│  └─ providers{}     → 7785 条渠道 + cost    │
└────────────────┬────────────────────────────┘
                 │ 主源
                 ▼
        ┌────────────────────┐
        │  canonical 索引    │  id → 元数据
        │  渠道索引          │  bare id → [(provider, cost)]
        └────────┬───────────┘
                 │
                 │  ID 精确 join (96.7%)
                 ▼
        ┌────────────────────┐        ┌──────────────────────┐
        │   模型清单（并集）  │◄───────│ llmpricing.dev       │
        │      1954 款       │  辅源  │  manifest + 5 分片   │
        └────────┬───────────┘        │  → AA / kind / min   │
                 │                    │  / blended / 渠道统计 │
                 │                    └──────────────────────┘
                 ▼
        ┌────────────────────┐
        │  ModelCatalogItem  │  46 字段完整
        │  → SQLite          │
        │  → get_model_catalog│
        └────────────────────┘
```

---

## 六、实施步骤

### 步骤 1：新增 models.dev 抓取器

在 `src-tauri/src/model/catalog/` 下新增抓取逻辑，替换现有分片循环（`catalog.rs` L1511–1542）。

- 单文件拉取 `https://models.dev/catalog.json`
- **必须带 ETag 条件请求**：响应头已提供 `etag`，配合 `cache-control: max-age=0, must-revalidate` 可避免重复传输 4.95 MB
- 4.95 MB 体积大于当前分片总量（1.62 MB），建议设超时与重试

### 步骤 2：建立双索引

```
canonical_index: HashMap<String, Value>        // canonical id → 元数据
provider_index:  HashMap<String, Vec<(String, Value)>>  // bare id → [(provider, model)]
```

`provider_index` 是价格聚合与字段兜底的基础。

### 步骤 3：实现 ID join

```rust
// 主键：canonical id 精确匹配
let md_meta = canonical_index.get(&llmpricing_id);
```

未命中时（13 款）走纯 models.dev 分支，`kind` / AA 字段留空，`status` 取 provider 层或默认 `ga`。

### 步骤 4：字段合并（主源优先）

```
for field in BASE_FIELDS:            // 15 项基础元数据
    item[field] = md_meta[field]     // 主源权威
    if item[field].is_none():
        item[field] = lp_row[field]  // 辅源兜底

for field in ENRICH_FIELDS:          // 17 项增强
    item[field] = lp_row[field]      // 辅源唯一来源
```

### 步骤 5：价格聚合

- `ref*` ← 从 `provider_index` 取官方渠道 `cost` 自行计算（已验证与 llmpricing 一致率 99.7%）
- `min*` ← **保留 llmpricing**（其过滤规则无法复现）
- `priceSpread` ← 保留 llmpricing

### 步骤 6：数据库与常量调整

| 位置 | 改动 |
|---|---|
| `catalog.rs` L13–14 | 新增 `MODELS_DEV_CATALOG_URL` 常量，保留 `LLMPRICING_*` |
| `catalog.rs` L15 | `CATALOG_SCHEMA_VERSION` 从 `"9"` 递增至 `"10"`（触发重建） |
| `catalog.rs` L300–355 | `model_catalog_sources` 表支持多源；新增 `source` 维度 |
| `catalog.rs` L395 / L709 / L952 | 硬编码的 `'llmpricing_manifest'` 改为参数化多源 |
| `catalog.rs` L404–534 | `parse_model_item_from_json` 拆分为「主源解析 + 辅源合并」两段 |
| `types.ts` L927–974 | 可选新增 `description` / `reasoningOptions` / `weights` / `license` / `links` / `benchmarks` 字段 |

### 步骤 7：失败降级

- models.dev 抓取失败 → 沿用上次缓存，标记数据陈旧
- llmpricing 抓取失败 → 基础元数据仍可用，AA/kind 留空，UI 需容忍空值
- 两源均可独立失败，互不阻塞

### 步骤 8：UI 调整（可选）

46 个字段全部保留，**UI 可以零改动**。若要利用新增字段，可考虑：
- 详情页展示 `description` 模型简介
- 详情页展示 `weights`（HuggingFace 链接）、`links`（论文链接）
- 表格/详情页展示「官方规格 vs 渠道最高」双 context 值

---

## 七、风险与注意事项

| 风险 | 等级 | 说明 | 缓解 |
|---|---|---|---|
| 误用 canonical 当清单 | 🔴 高 | 会丢 1559 款模型 | 强制并集，加回归测试断言模型数 ≥ 1941 |
| `min` 口径变化 | 🟠 中 | 若改用 models.dev 计算，最低价会变成 0（含免费渠道） | 保留 llmpricing 的 `min` |
| `context` 显示值变化 | 🟠 中 | 70/382 模型显示值会变（官方规格 vs 渠道值） | 按 3.3 节并存设计，避免覆盖 |
| provider 级定位歧义 | 🟠 中 | bare id 可能对应多家供应商 | 优先取官方渠道，其次取 `ref` 渠道 |
| 长尾模型无主源数据 | 🟡 低 | 1559 款仅 llmpricing 有；其中 100% 能按 name 命中 provider 级，但 163 个有歧义 | 保持现状，不强行匹配 |
| models.dev 无版本化 API | 🟠 中 | 字段可能静默变更 | 加 schema 校验 + 抓取失败告警 |
| 单文件体积增大 | 🟡 低 | 4.95 MB vs 1.62 MB | ETag 条件请求 |

### 备选方案（不推荐，但值得知道）

models.dev provider 层有 **2524 个唯一 bare id**，其中 **1027 个不在 llmpricing 里**。理论上可完全从 provider 层自建 canonical，覆盖上限约 2524 款，比并集方案的 1954 款更多。

**但需要自研「渠道模型 → canonical 归并」算法**（判断哪些渠道模型属于同一款），这正是 llmpricing 的核心工作，复刻风险高、易产生偏差。**建议先落地并集方案，把自建 canonical 作为后续演进方向。**

---

## 八、上线前验证清单

- [ ] 模型总数 ≥ 1941（并集生效，未误用 canonical）
- [ ] 382 款双源模型的 `refInputCost` 与旧值一致率 ≥ 99%
- [ ] 13 款新增模型能正常展示，`kind`/AA 字段为空不报错
- [ ] 1559 款长尾模型的字段与迁移前完全一致
- [ ] AA 相关 6 项 UI（列 / 筛选 / 排序 / 指标卡 / 仪表盘）行为不变
- [ ] `min` 最低价与旧值一致（确认未被 models.dev 免费渠道污染）
- [ ] 两源独立失败降级正常，不出现空白页
- [ ] `CATALOG_SCHEMA_VERSION` 已递增，旧数据正确重建
- [ ] ETag 条件请求生效，二次同步无重复下载

---

## 九、一句话总结

**这个架构成立。** 两源 ID 同命名空间、96.7% 精确匹配，`ref` 价格一致率 99.7%，models.dev 还能净新增 `description` / `reasoning_options` / `interleaved` / `weights` / `license` / `links` 等字段。真正无法替代的只剩 `kind` 和 6 个 AA 评测分。**前提是必须取并集（1954 款），绝不能直接用 models.dev 的 395 条 canonical 列表。**

---

## 附录：验证数据汇总

| 指标 | 数值 |
|---|---|
| models.dev catalog.json | 4,954,702 B |
| models.dev 供应商 / canonical / 渠道记录 | 213 / 395 / 7785 |
| llmpricing 供应商 / canonical / 渠道记录 | 213 / 1941 / 7780 |
| ID 精确匹配 | 382 / 395（96.7%） |
| 并集模型数 | 1954（净增 13） |
| `ref` 价格一致率 | 301 / 302（99.7%） |
| `min` 价格一致率 | 197 / 330（59.7%） |
| `context` 值差异 | 70 / 382（18.3%，语义差异） |
| `outputLimit` 值差异 | 82 / 378（21.7%，语义差异） |
| name 差异 | 6 / 382（1.6%） |
| provider 层 bare id 唯一数 | 2524（其中 1027 不在 llmpricing） |

代码依据：
- `src-tauri/src/model/catalog/catalog.rs` L13–15（常量与版本）、L300–355（表结构）、L404–534（解析）、L1511–1542（同步）
- `src/types.ts` L908–1018（类型定义）
- `src/components/pages/ModelCatalogPage.vue` L301–310、L136–278、L1559–1580（UI 依赖）
