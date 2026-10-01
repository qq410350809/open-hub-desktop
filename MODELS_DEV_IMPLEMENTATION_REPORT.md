# models.dev 主数据源改造：实现完成报告

> 承接 `MODELS_DEV_MIGRATION_FEASIBILITY.md` / `MODELS_DEV_PRIMARY_ARCHITECTURE.md` /
> `MODELS_DEV_OFFICIAL_VS_THIRD_PARTY.md` 三份设计文档，本文档记录**实际落地结果**与实测数据。

## 一、总体结论

| 项目 | 改造前 | 改造后 |
|---|---|---|
| 数据源 | llmpricing 单源 | **models.dev 主源 + llmpricing 辅源** |
| 渠道明细获取 | HTML 爬取 ~1800 个详情页 | **组合推导，0 次详情页请求** |
| 单次同步请求数 | ~1806 次 | **7 次**（manifest + 5 分片 + catalog.json） |
| 单次同步流量 | ~440 MB | **~6.6 MB** |
| 原始 lab / 原始 modelId | 无（`lab` 有 175 个 `misc`） | 有，`misc` 降至 72 个 |
| 官网 / 三方分层 | 无 | `officialHostCount` + tier 三分 |
| 免费渠道 | 只能拿到聚合计数 | 聚合计数 + **逐渠道明细** |

## 二、新增模块

### `src-tauri/src/model/catalog/models_dev.rs`（约 640 行）

models.dev 数据源。抓 `https://models.dev/catalog.json`（4.9MB，带 ETag 条件请求），
同时解析 **canonical 层**（395 个模型，`lab/modelId`）与 **provider 层**
（213 个渠道 / 7787 条渠道记录）。

关键导出：

- `normalize_model_id` —— 跨层 ID 归一化（**已验证与 Python 参考实现 3910/3910 完全一致**）
- `ModelsDevIndex` —— `canonical` / `provider` / `canonical_by_normalized` / `provider_meta` 四张索引
- `channels_for_with_free_fallback` —— 带 `-free` 回退的渠道查找
- `channels_grouped_by_provider` —— 按渠道分组的全部变体
- `fetch_catalog_raw` —— ETag 条件请求（304 时省下 4.9MB）

**命名空间陷阱（务必牢记）**：

| 层 | 键形态 | 示例 |
|---|---|---|
| canonical | `lab/modelId` | `zhipuai/glm-5.2` |
| provider | `provider/modelId` | `subconscious/glm-5.2`、`@cf/deepseek-ai/...` |

provider 层拼写由各渠道自行声明，极不统一（`glm-5.2` / `z-ai/glm-5.2` / `meta/llama-3.3-70b` /
`llama3.3:70b`），跨层匹配必须走归一化。

**另一个坑**：provider 层条目里的 `provider` 字段**不是渠道名**，而是该渠道对该模型的
**请求形态覆盖对象**（`{npm, api, shape}`）。不要当渠道归属用。

### `src-tauri/src/model/catalog/lab_registry.rs`（约 420 行）

模型身份（原始 lab / 原始 modelId）与原厂渠道判定。

核心判定：

```text
is_official(model, provider) = provider ∈ official_hosts(model.lab)
                             ∧ provider 确实上架了该模型
```

「上架校验」由 models.dev provider 层提供，因此**无需爬取**。

#### ⚠️ 为什么不能直接用 `tier`

llmpricing 的 `tier`（`lab` 31 / `cloud` 39 / `gateway` 143）描述**渠道自身性质**，
与「是否为某模型的原厂渠道」正交。实测反例：

- `nvidia` 的 `tier = lab`，却代售 30+ 个 lab 的模型
- `alibaba-cn` 的 `tier = lab`，同时上架 deepseek / minimax / moonshotai 的模型
- `azure` 的 `tier = lab`，对上架 `microsoft/phi-*` 是官方，对上架 `openai/*` 却**不是**

#### ⚠️ `official` 是模型级的，不是 lab 级的

- `openai/gpt-oss-120b` 的开源权重模型**不在** `openai` 渠道上架 → 官方渠道集合为空
- `amazon/nova-2-lite` 的 `amazon-bedrock`、`bytedance-seed/seed-2.0-code` 的 `volcengine`
  都被上游判为**非官方**

因此 `amazon` 与 `bytedance-seed` 在别名表中**显式留空**，防止误配。

#### 已验证的原厂别名表（23 个 lab）

| lab | 原厂渠道 |
|---|---|
| alibaba | `alibaba`, `alibaba-cn` |
| anthropic | `anthropic` |
| cohere | `cohere` |
| deepseek | `deepseek` |
| google | `google`, `google-vertex` |
| meituan | `longcat` |
| meta | `llama`, `meta` |
| microsoft | `azure`, `azure-cognitive-services` |
| minimax | `minimax`, `minimax-cn` |
| mistral | `mistral` |
| moonshotai | `moonshotai`, `moonshotai-cn` |
| nvidia | `nvidia` |
| openai | `openai` |
| perplexity | `perplexity` |
| poolside | `poolside` |
| sakana | `sakana` |
| stepfun | `stepfun`, `stepfun-ai` |
| tencent | `tencent-tokenhub` |
| thinkingmachines | `thinkingmachines` |
| xai | `xai` |
| xiaomi | `xiaomi` |
| zhipuai | `zhipuai`, `zai` |
| amazon / bytedance-seed | **（空）** |

#### `misc/*` 归属：三级判定，不猜

llmpricing 有 175 个模型 `lab = misc`。判定顺序：

1. **关键词规则**（26 条，前缀/包含匹配）→ 判定 100 个
2. **同名继承**（llmpricing 内部同名行且 lab 唯一）→ 再判 3 个
3. 其余 72 个保持 `misc`，`identityResolved = false`

剩余 72 个是白牌 / 路由 / 聚合名（`auto`、`auto-model-*`、`model-router`、`custom`、
`free`、`echo`、`claw-*`、`code-*`、`text-*`、`agent-*`、`neosmith.*`、`lucidquery-*`、
`kloker*`、`omen-alpha`、`fugu-ultra-v1-0` …）。**刻意不猜**——价格对比要可信，
宁可标「未确定」。

### `src-tauri/src/model/catalog/channel_builder.rs`（约 560 行）

用「models.dev 渠道层 + llmpricing 范围」组合构建渠道明细。

字段来源分工：

| 字段 | 来源 |
|---|---|
| 有哪些渠道（范围） | llmpricing 分片 `hostProviders` |
| `input`/`output`/`cacheRead`/`cacheWrite` | models.dev provider 层 `cost` |
| `context`/`outputLimit` | models.dev provider 层 `limit` |
| `modelId` 原始拼写 | models.dev provider 层模型键 |
| `reasoningOptions`/`experimental`/`status` | models.dev provider 层同名字段 |
| `tier`/`subscription`/`doc` | llmpricing manifest |
| 是否官方渠道 | `lab_registry` 别名表 + 上架校验 |
| 免费/订阅**聚合计数** | llmpricing 分片 `freeHostCount`/`subHostCount`（权威值，直接沿用） |

#### ⚠️ 两条必须遵守的判定规则

1. **免费 = 任一变体零价**。同一渠道可能同时登记 `glm-5.2`（付费）与 `glm-5.2:free`（零价），
   上游按零价那条计为免费渠道。只取「主变体」会漏报（实测 `unorouter` 即此类）。
2. **订阅集必须剔除 `github-copilot`**。它在 manifest 里带 `subscription: true`，
   但上游 `subHostCount` 不计它。不剔除会使一致率从 99.8% 掉到 98.4%。

## 三、实测保真度

对照 llmpricing 上游聚合值，全部 1941 个模型：

| 指标 | 结果 |
|---|---|
| 官方渠道识别（23 个 lab 抽样 vs 详情页 `official`） | **23/23 = 100%** |
| 归一化函数 vs Python 参考实现（3910 个 ID） | **3910/3910 = 100%** |
| `subHostCount` 完全一致 | **1937/1941 = 99.8%** |
| 免费渠道**计数**完全一致 | **1890/1941 = 97.4%** |
| 「该模型是否有免费渠道」**二值**一致 | **1923/1941 = 99.1%**（多报 2 / 漏报 16） |

剩余 16 个漏报是 **models.dev 与 llmpricing 的真实数据差异**，不可归一化消除：

- llmpricing 额外直采 openrouter.ai，且部分模型用日期命名
  （llmpricing `mistral-medium-2604` vs models.dev `mistral-medium-3.5`）
- Amazon Bedrock 命名带区域前缀（`us.amazon.nova-2-lite-v1:0`）

**关键设计决策**：保留 llmpricing 分片自带的 `freeHostCount` / `subHostCount` 作为
**权威聚合值**，models.dev 只负责回答「是哪些渠道」。两者不一致时以聚合值为准，
并用 `freeChannelCountMatches` 标记口径差异。

## 四、真实端到端验证结果

`cargo test --lib sync_model_catalog_end_to_end -- --ignored --nocapture`

```
同步完成：providers=213 models=1941 shards=5
模型总数            = 1941
身份已确定          = 1869     （1941 - 72 个不可判定的白牌/路由名）
定位到 canonical    = 542
有官方渠道          = 382
有免费渠道(本地推导)= 316
有免费渠道(上游聚合)= 330
免费计数与上游一致  = 1890  (97.4%)
免费渠道来源=derived= 1941
有渠道范围          = 1941
```

`glm-5.2` 抽样：

```
officialLab       = zhipuai
officialModelId   = glm-5.2
canonicalId       = Some("zhipuai/glm-5.2")
identitySource    = canonical
officialHostCount = 2
tier 分布         = lab 6 / cloud 22 / gateway 59
freeChannelCount  = 4 (上游 freeHostCount = 4)
freeChannels      = ["kenari", "nvidia", "sensenova", "unorouter"]
subscriptionChans = ["alibaba-token-plan", "alibaba-token-plan-cn", "scnet-token-plan",
                     "umans-ai-coding-plan", "zai-coding-plan", "zhipuai-coding-plan"]
hosts 条数        = 87      （与 llmpricing hostProviders 的 87 完全一致）
官方渠道          = ["zai", "zhipuai"]   ← 与详情页 ground truth 一致
```

## 五、表结构与类型变更

`CATALOG_SCHEMA_VERSION` 从 `"9"` 升到 `"10"`（触发 DROP 重建）。

### `model_catalog_models` 新增 14 列

```
official_lab                        TEXT    归一化后的原始 lab
official_model_id                   TEXT    原始 modelId（保留上游原拼写）
canonical_id                        TEXT    models.dev canonical id
identity_source                     TEXT    canonical|llmpricing_lab|keyword_rule|inherited|canonical_reverse|unknown
identity_resolved                   INTEGER 身份是否已确定
official_host_count                 INTEGER 官方渠道数
lab_tier_host_count                 INTEGER tier=lab 的渠道数
cloud_tier_host_count               INTEGER tier=cloud 的渠道数
gateway_tier_host_count             INTEGER tier=gateway 的渠道数
free_channel_count                  INTEGER 本地推导的免费渠道数
free_channel_providers_json         TEXT    免费渠道 id 列表
subscription_channel_providers_json TEXT    订阅渠道 id 列表
free_channel_source                 TEXT    derived|upstream
free_channel_count_matches          INTEGER 是否与上游聚合值一致
```

新增索引：`idx_model_catalog_models_official_lab`、`idx_model_catalog_models_free_channel`。

### `model_catalog_providers` 新增 1 列

```
is_first_party  INTEGER  该渠道是否为某个 lab 的自营渠道
```

### `src/types.ts`

`ModelCatalogProvider` 加 `isFirstParty`；`ModelCatalogItem` 加 14 个字段（含中文注释）。
`src/composables/core/mockData.ts` 同步补齐。

## 六、⚠️ 踩到的坑：schema 定义重复

`src/core/db.rs:194` 的迁移里**也有一份** `CREATE TABLE IF NOT EXISTS model_catalog_models`
的旧定义，且它先于 catalog 模块执行。若 `persist_catalog_llmpricing` 只调
`ensure_catalog_schema`，`IF NOT EXISTS` 会因为表已存在而变成**空操作**，新增列永远建不出来。

**修复**：`persist_catalog_llmpricing` 改为调用 `clear_legacy_catalog_if_needed`，
按 `CATALOG_SCHEMA_VERSION` 对账并 DROP 重建，使 catalog.rs 的 DDL 成为唯一权威。
并在 `core/db.rs` 该处加了警示注释。

> 这个 bug 是靠**真实端到端同步测试**抓到的，单元测试完全测不出来。

## 七、被移除的代码

- `fetch_hosts_for_model`（HTML 详情页抓取）
- `prefetch_model_hosts`（并发 8 预拉取 ~1800 页）
- `extract_hosts_from_html` / `collect_rsc_flight` / `find_array_string_aware` /
  `find_array_naive` / `decode_json_string_literal`（RSC flight 解析）
- 对应 3 个 HTML 解析测试
- `persist_catalog_llmpricing` 里的 `hosts_cache` 快照/恢复逻辑（渠道明细每次重建，无需缓存）
- `get_model_catalog_detail_inner` 里的按需回源抓取

## 八、新增的回归测试（3 个，均 `#[ignore]`，需联网/需本地快照）

| 测试 | 作用 |
|---|---|
| `golden_catalog_dump` | 导出全部 ID 的归一化结果供逐行 diff；校验真实索引构建；**校验官方渠道 23/23** |
| `golden_free_channel_fidelity` | 全量保真度自查，含**门槛断言**（sub≥99%、free≥96%、二值≥98.5%） |
| `sync_model_catalog_end_to_end` | 真实跑一遍完整同步并校验产出 |

运行方式见各测试的文档注释。

## 八之二、⚠️ 线上事故与根治（必读）

### 事故现象

模型参数页整页空白，报：

```
模型参数同步失败: no such column: subscription_channel_providers_json in
SELECT id, slug, name, ... FROM model_catalog_models ORDER BY ...
```

### 根因

`CATALOG_SCHEMA_VERSION` 停在 `"10"`，而
`subscription_channel_providers_json` / `official_channel_providers_json` 是 `"10"`
**发布之后**才加的两列。

旧实现的重建条件是「**版本号不一致**」：

```rust
if schema_version != CATALOG_SCHEMA_VERSION { drop_and_recreate(); }
```

于是对已存在的库：版本号 `"10"` == 当前 `"10"` → 判定「无需重建」→ 表里永远没有新列
→ SELECT 直接失败 → 整个目录页空白。

**本质问题是「靠人记得 bump 版本号」这个约定不可靠**——我在同一次任务里加了两次列，
第二次就漏了。

### 根治：DDL 指纹自动对账

把 DDL 抽成常量 `CATALOG_SCHEMA_DDL`，对**字符串本身**取 64 位 FNV-1a 指纹并落库。
重建条件改为「版本号不一致 **或** 指纹不一致」：

```rust
let version_changed = schema_version != CATALOG_SCHEMA_VERSION;
let ddl_changed = stored_fingerprint != current_fingerprint;
if version_changed || ddl_changed { drop_and_recreate(); }
```

这样**只改 DDL、忘了 bump 版本号也会自动重建**。版本号退化为「给人读的语义标签」，
不再承担正确性责任。

### 配套：把「改了一边忘了另一边」变成测试失败

新增 `select_columns_match_ddl_columns`，双向对齐 `SELECT` 清单与 `DDL` 列：

| 方向 | 能抓到的问题 |
|---|---|
| SELECT ⊆ DDL | SELECT 引用了 DDL 中不存在的列（**正是本次事故**）|
| DDL ⊆ SELECT ∪ 白名单 | DDL 加了列但忘了加进 SELECT（会**静默漏读**，比报错更危险）|
| 列数固定断言 | 改列数但忘了同步 `read_model_row` / `read_provider_row` 的下标 |

`read_*_row` 用**下标**取值，所以这个守卫尤其重要。

### 新增回归测试（6 个，均不联网）

| 测试 | 作用 |
|---|---|
| `select_columns_match_ddl_columns` | SELECT/DDL 双向对齐 + 列数守卫 |
| `stale_table_is_rebuilt_even_when_version_matches` | 复现事故：版本号一致但表缺列 → 必须重建 |
| `changed_ddl_with_same_version_triggers_rebuild` | 版本号一致但指纹不匹配 → 必须重建 |
| `up_to_date_schema_is_not_rebuilt` | 结构已最新时**不应**无谓清空数据（与上条配对）|
| `schema_fingerprint_changes_with_ddl` | 指纹对 DDL 内容敏感且稳定 |
| `persist_preserves_models_dev_source_row` | persist 不能抹掉 models.dev 来源行 |

**全部做过变异测试**（故意改坏实现，确认测试真的会失败），不是摆设。

### 配套：models.dev 来源行被误删

修复过程中发现另一个自引入缺陷：`model_catalog_sources` 里**没有 models.dev 来源行**。

两处把它抹掉了：

1. `persist_catalog_llmpricing` 无条件 `DELETE FROM model_catalog_sources`；
2. 更隐蔽的是 `persist` 内部调的 `clear_legacy_catalog_if_needed` 在首次同步时
   **DROP 掉整张表**——而 `fetch_models_dev_index` 已经在此之前写入了来源行。

修法：

- persist 改为 `DELETE ... WHERE source != 'models_dev_catalog'`；
- `fetch_models_dev_index` **入口先对账 schema**，后续 persist 的调用就不会再重建；
- 304 / 缓存路径补 `ensure_models_dev_source_row`（`INSERT OR IGNORE`，不刷新
  `fetched_at`——它表示最后一次成功联网抓取时间）；
- `is_synced_today` 追加「models.dev 来源行必须存在」，否则视为**不完整快照**并重新同步，
  用户不必手动点刷新。

### 事故二：`models.dev catalog 返回 304 未变更，且本地无缓存可用`

修完上面那个 bug 后紧接着暴露的第二个问题，**根因是 ETag 与正文分居两张表**：

| 内容 | 存放位置 | schema 变更时 |
|---|---|---|
| ETag | `app_meta.model_catalog_models_dev_etag` | **保留**（app_meta 不参与重建）|
| 正文 | `model_catalog_sources.raw_json` | **被 DROP** |

于是必然出现「**孤儿 ETag**」：手里有 ETag、没有正文，却仍然发条件请求 →
服务端回 `304 Not Modified` → 无任何东西可重建索引 → 同步直接失败。

触发路径：旧的 persist 把 models.dev 来源行删掉（事故一的副作用），
留下一个孤儿 ETag；下一次同步就撞上 304。

**三层修复**（缺一不可）：

1. **没有正文就不发条件请求** —— 抽成纯函数 `conditional_etag_for(etag, has_cached_raw)`，
   规则显式化、可单测。
2. **schema 重建时同时作废 ETag** —— 两者同生共死，不再产生孤儿。
3. **兜底重试** —— 万一仍收到 304 而无缓存（ETag 与正文来自不同代），
   去掉条件头完整重抓一次，而不是把同步判失败。

> 教训：**跨两张表存一对有依赖关系的数据，就必须保证它们的生命周期一致**，
> 否则一定会出现「一半还在、一半没了」的中间态。

### 新增回归测试（4 个，均不联网）

| 测试 | 作用 |
|---|---|
| `schema_rebuild_invalidates_models_dev_etag` | 重建后 ETag 必须被作废 |
| `conditional_request_requires_cached_body` | 有 ETag 无正文时不得发条件请求 |

**全部做过变异测试**（故意改坏实现，确认测试真的会失败），不是摆设。

### 顺带补的前端展示

`ModelCatalogPage.vue` 新增「数据来源」卡片（此前 `sources` 字段前端**完全没用到**）：
`models.dev N 模型`（主源高亮）/ `llmpricing N 渠道` / `N 个分片`；
主源缺失时显示橙色告警徽章而非静默少数据。

### 真实库验证

对线上库 `~/Library/Application Support/com.dfeer.openhub.desktop-dev/sites.sqlite3`
（只读）核对：

```
model_catalog_schema_version     | 11
model_catalog_schema_fingerprint | fd3f2f1a2fb72d34
新列                             | official_lab / official_model_id / official_host_count
                                 | official_channel_providers_json / free_channel_providers_json
                                 | subscription_channel_providers_json  全部存在
模型行数                         | 1941
身份已确定 / misc / 有 canonical  | 1869 / 72 / 542
有官方渠道 / 有免费渠道 / 计数一致 | 382 / 316 / 1890
供应商 is_first_party            | 30 原厂 / 183 三方
glm-5.2                          | official_lab=zhipuai, 官方渠道=["zai","zhipuai"],
                                 | 免费 4 个=["kenari","nvidia","sensenova","unorouter"]
```

全部与离线分析、端到端测试的预期值一致——**库已自愈且数据正确**。

## 九、前端接入（已完成）

`src/components/pages/ModelCatalogPage.vue` 已接入全部新字段。改动要点：

### 9.1 列表页

- **新增「官网 / 三方构成」列**：显示 `官方 N | 三方 M`，并在有官方价基准时给出
  `三方省 X%`。悬停可看具体官方渠道清单。
- **新增「渠道构成」筛选下拉**：有原厂官方渠道 / 纯三方（无官方渠道）/ 仅原厂渠道上架 /
  有订阅制渠道 / 厂商身份未确定。
- **新增排序「三方比官网省幅（从高到低）」**：无法计算（无官方价）的排到最后。
- **厂商筛选与展示改用 `officialLab`**（归一化后的原始 lab），否则 175 个 `misc/*`
  模型会全部挤进「开源社区」一项。身份未确定的模型在厂商列追加「身份未定」标记。
- 搜索关键词扩展到 `officialLab` / `officialModelId` / `canonicalId` /
  官方渠道 / 免费渠道。

### 9.2 详情抽屉

新增「官网 / 三方渠道分层」卡片，包含：

1. **身份条**：原始厂商 (lab) / 原始模型 ID / canonical 标识 / 身份来源
2. **身份未确定警告条**（`identityResolved === false` 时）：
   说明「系统保留 `misc` 而非猜测」
3. **三方价格对比三栏**：
   - 原厂官方直销价（无官方渠道时显示「该模型原厂未直接上架」）
   - 三方渠道最低价
   - 三方相对官网省幅
4. **渠道构成计数**：原厂官方 / 三方 / 免费 / 订阅
5. **渠道性质三分**（`tier`：自营 / 云厂商 / 聚合网关）
6. **官方 / 免费 / 订阅渠道 chips** 明细清单
7. **口径差异提示**（`freeChannelCountMatches === false` 时）

### 9.3 渠道明细表

- 新增「性质」列（官方 / 三方）
- 新增「仅显示原厂官方渠道」开关，可一键筛出官网报价与三方对照

### 9.4 修复的历史缺陷

1. **`createInitialDetail` 把订阅渠道误标为免费**
   （原 `isFree: ... || p?.subscription`）→ 改为读后端推导的 `freeChannelProviders`。
2. **详情头部免费计数把订阅渠道算作免费**
   （原 `h.isFree || (h.input === 0 && h.output === 0)`）→ 改为只看 `h.isFree`。
3. **`official` 由 `refProvider` 反推**（原 `isRef && model.refOfficial`）
   → 改为读后端推导的 `officialChannelProviders`。
4. **价格卡自相矛盾**：「暂无三方报价」与「省 17%」并存（两处用了不同基准）
   → 改为同源计算 `detailOriginSummary.saving`。

### 9.5 暗色主题适配

新增样式**全部使用 CSS 变量**；边框/文字对比度用 `color-mix(in srgb, var(--x) N%, var(--line))`
而非硬编码浅色，保证 `:root[data-theme="dark"]` 下依然可读。

### 9.6 预览数据（`mockData.ts` / `browserFallback.ts`）

- `previewProviders` 补 `isFirstParty`
- 7 个静态模型补 15 个新字段，并新增 1 个 `misc/model-router-pro` 样本用于演示
  「身份未确定」场景
- 生成器（94 个模型）的字段改为**按 index 动态生成**，不再硬编码
- `browserFallback` 的详情构造与真实后端语义对齐

### 9.7 验证方式

用 headless Chrome + CDP 实际渲染并交互验证（不只是类型检查）：

| 验证项 | 结果 |
|---|---|
| 表头含「官网 / 三方构成」 | ✓ |
| 首行渲染 `官方 1 \| 三方 27 \| 三方省 17%` | ✓（1−1.25/1.50 ≈ 17%，数学正确）|
| 详情卡片 7 个区块全部渲染 | ✓ |
| 无官方渠道时显示「原厂未直接上架」+ 省幅「—」 | ✓ |
| 身份未确定时警告条渲染（暗色下可读） | ✓ |
| 「仅显示原厂官方渠道」把 4 行筛到 1 行 | ✓ |
| `vue-tsc`（含模板表达式检查） | 0 个新增错误 |
| `vite build` | 通过 |

## 十、横向对比 Arena / 算费器 / 供应商矩阵接入（已完成）

### 10.1 月度用量算费器：两档 → **三档**

原实现只有「官方参考 / 最低渠道」两档，且最低渠道价可能就来自官方渠道，
会得出「三方比官网便宜」的**假结论**。现改为：

| 档位 | 取价规则 |
|---|---|
| 原厂官方直销月费 | `refInputCost`/`refOutputCost`，并标注是否为真正的原厂渠道（否则标「参考价口径」）|
| 三方渠道最低月费 | `minInputCost`/`minOutputCost`，**但若最低价渠道本身就是官方渠道则置空**并提示「最低价即官方渠道」|
| 免费渠道月费 | 存在免费渠道时显示 `¥0.00` + 「N 家免费渠道 · 通常有限速或配额」|
| 三方相对官网每月节省 | `官方档 − 最低付费档` |

### 10.2 Arena 横向对比：新增 5 行

- **原始厂商 / 模型标识**（`officialLab` + `officialModelId`，未确定时标注）
- **原厂官方价格 (/1M)**（无官方渠道时显示「无官方渠道」）
- **官网 / 三方渠道数**（`官方 N | 三方 M` 徽章）
- **三方相对官网省幅**
- **免费渠道**（数量 + 渠道名，最多列 3 个）

同时表头与卡片/表格/抽屉的厂商展示统一改用 `effectiveLab()`。

### 10.3 供应商拓扑矩阵

- 卡片新增「原厂自营 / 三方」徽章（基于 `ModelCatalogProvider.isFirstParty`）
- 新增「仅显示原厂自营渠道」开关（实测 8 家 → 4 家）

### 10.4 实机验证（headless Chrome + CDP）

| 验证项 | 结果 |
|---|---|
| 算费器渲染 4 档 | ✓ 原厂 ¥195.75 / 三方 ¥163.13 / 免费 ¥0.00 / 省 ¥32.63 (17%) |
| 数字正确性 | ✓ 10M×$1.5 + 2M×$6 = $27 → ¥195.75；省 $4.5 → ¥32.63 = 16.7% ≈ 17% |
| Arena 新增 5 行全部渲染 | ✓ `-17%` / `-10%`、`官方 1 \| 三方 27`、免费渠道名列 |
| 供应商矩阵原厂过滤 | ✓ 8 家 → 4 家（OpenAI/Anthropic/Google/DeepSeek）|
| `vue-tsc` | 0 个新增错误 |
| `vite build` | 通过 |

## 十一、后续可做（未纳入本次范围）

1. `model_catalog_sources` 表已有 models.dev 来源行，前端「数据来源」面板可展示
   canonical 数 / 渠道数 / 抓取时间与 ETag 命中情况。
2. 算费器可增加「缓存读取命中率」输入项（`cacheRead` 已入库但未参与计算）。
3. Arena 可增加「按官方渠道 / 按三方渠道」两套总价估算列。




---

## 十二、Agent 配置逐模型默认参数：目录引用

### 已实现的规则

最大窗口、最大输出、思考档位集合与**默认思考级别**都默认引用模型全景控制台同一份本地目录。
默认思考级别取该模型**最高可配置档位**（声明档位集合的最高档；集合缺失时用后端算好的
`reasoningEffortMax`）；未命中、仅 toggle 或不支持思考的模型不臆造默认档。

```
逐字段子级自定义 > 同模型父级显式覆盖（仅模型×站点模式）
  > 目录能力 > 配置文件回读 > Agent 全局快照
```

- `modelParams.ts` 是前端字段解析、档位归一化、分组键和磁盘回读的共享纯函数。
- 仅保存用户显式修改的字段；改窗口不会把输出、档位等默认值一起固化。
- `undefined` 表示恢复继承；`0`、`null`、空字符串、空数组属于显式值。
- 读取磁盘只重建 `hydratedOverrides`，不删除 `childOverrides`。即使自定义值恰好
  等于目录或磁盘，也不能据此推断为“旧默认”而吞掉。
- 旧版 preferences 没有可靠来源标记，保守保留已有覆盖；用户可用“恢复默认”或
  “清除父级覆盖”显式清除。这可能需要对旧版已固化默认值操作一次。
- 逐模型默认思考级别会随 patch 的 `models[].reasoningEffort` 下发；OpenCode 写入
  `options.reasoningEffort`（并要求 `reasoning: true`），其它适配器不消费该字段。
- OpenCode 配置文件没有全局思考级别字段：全局档仅在保存时作为输入扇出到各模型的
  `options.reasoningEffort`，读回恒定「未设置」——从任意模型反推全局会把逐模型值
  误当成全局设置，配置比对也会误报不一致；磁盘上的真实档位由各模型逐条承载。
- 页面回显与 patch 使用同一解析函数和同一分组键；空档位会显式清除旧映射。
- 已知不支持 effort / 仅 toggle 的模型不再自动填满全部 effort 档位。

### 匹配与缓存

`nearest.rs` 使用独立、安全的身份键，不修改目录同步的 `normalize_model_id`：
强精确匹配优先，再使用受约束的拼写标准化、已知日期/effort 变体和厂商限定别名。
版本、尺寸、词序以及 thinking/turbo/flash/mini/codex/max/fast 等语义差异不得靠
相似度消除。同一级多个不同目录候选时拒绝任取首项；无法证明唯一时保持未匹配。
接口继续返回 `matchedId / matchedModelId / matchKind / matchScore / matchVersion=2`，
非精确来源在页面以 `≈目录` 披露；分数不是正确率。

能力缓存按请求原始 ID 保存，前端不再做第二次弱归一化匹配；目录刷新会失效缓存，
epoch 守卫阻止旧响应覆盖新响应。显示“目录未匹配”与请求失败是两个独立状态。
生效和差异比对都会等能力加载完成；请求失败中止写盘，不静默写旧默认。
生效操作从预加载阶段加锁，切换工具后的旧读盘/保存响应不得覆盖当前工具。

### 各 Agent 的实际边界

| Agent | 逐模型窗口/输出 | 逐模型思考档位集合 | 默认思考级别 |
|---|---|---|---|
| OpenCode | 写入 `limit` | 写入 `variants` | 写入逐模型 `options.reasoningEffort`（默认=目录最高档） |
| DSH | 写入 `contextWindow/maxTokens` | 写入并回读 `reasoningEfforts` | 无逐模型默认档写入 |
| ZCode | 写入 `limit` | 当前适配器不写 | 当前适配器不写 |
| Claude / Codex | 仅全局项，不消费逐模型 limits | 不消费；Claude 的 map 是三档模型映射 | 全局设置 |

页面按字段标注来源（目录 / 父级自定义 / 配置文件 / Agent 默认）；逐模型与父级「默认思考级别」
都可编辑，留空即跟随目录最高档。不会仅因打开页面或目录同步自动改写用户的 Agent 文件。

### 验证方式

- `npm run test:agent-model-params`：加载真实 TypeScript 模块的 24 项离线回归，含
  字段优先级、稀疏覆盖、空值、回读不吞覆盖、目录更新和负向控制。
- 隔离 Chrome + 内存 IPC：验证目录默认、编辑、保存回读、切换 Agent、恢复默认、
  目录更新、模型父级/patch 一致、空档位、请求失败阻止写入及等待新目录。
- Rust 目录匹配与适配器测试使用合成目录/临时配置；没有运行真实 Agent 配置写入。
- 不把少量真实库拼写探针视为全量命中率或零误配证据。
本轮最终验证：`vue-tsc --noEmit` 与 Vite 构建通过；目录模块 57 passed / 3 ignored，
local_tools 30 passed（含逐模型默认思考级别进入版本比对）；前端纯函数回归 24 passed。
隔离浏览器额外验证连续点击只写一次、整页重载保留覆盖，以及预加载期间离开页面取消写入，均通过且无运行时异常。
全库在空 `PI_DESKTOP_HOME` 下运行：509 passed / 5 ignored；
`workspace_project_key_normalization_rules` 因 scratch 临时路径与其 `/tmp` 假设不同失败，
未修改该无关测试，显式排除后其余 509 项通过（1 filtered out）。
### 修复：旧版后端导致「整屏未设」（2026-09-19 复现截图）

症状：模型×站点模式下父级栏与逐模型行都显示「默认: 未设」，思考档位回到全集。
根因：前端热更新而后端二进制未重编译，`get_model_capabilities` 不返回 `matchVersion`；
当时的版本校验把整批载荷判为失败并丢弃，于是所有目录默认值消失。

修正：`matchVersion` / `matchedId` / `matchKind` 降级为**仅供披露**，取值只用窗口、输出、
档位字段；元信息缺失时照常显示「目录 · <模型>」并提示「当前后端未返回匹配来源信息」。
父级栏改为显示「值 + 来源」：`目录: 200K` / `配置文件: 128K` / `未匹配 · 用 Agent 默认`。
隔离浏览器新增旧版载荷场景：目录窗口 / 输出必须显示，且「生效」必须写入这两个字段。

⚠️ 部署提示：**改 Rust 后必须重启或重编译桌面端**（`npm run desktop` / `desktop:build`），
只跑前端热更新不会让目录匹配（`≈` 变体 / 渠道别名 / 近似）生效。
