# 模型身份归一化 + 官网/三方渠道分离 —— 设计方案与实施计划

> 需求：官网与三方渠道分开；能拿到**原始 lab** 与**原始 model id**，使价格对比与模型请求分析有意义。
> 承接《MODELS_DEV_PRIMARY_ARCHITECTURE.md》
> 分析时间：2026-09-14

---

## ⚠️ 重要修正（2026-09-14 追加）

深入核查 llmpricing 的**模型详情页**（`https://llmpricing.dev/m/{id}/`）后，本方案需要修正两处：

### 修正一：llmpricing 详情页已提供正确的 `official` 标志

抓取 `llmpricing.dev/m/zhipuai/glm-5.2/` 实测，其内嵌 `hosts` 数组每条含：

```
provider, modelId, input, output, cacheRead, cacheWrite,
reasoning, context, outputLimit, status, official,
tiers, reasoningOptions, experimental
```

其中 **`official` 字段已正确计算**——GLM-5.2 的 102 条渠道中，只有 `zhipuai` 和 `zai` 为 `official: true`，与人工判断完全一致。

**因此第 4.1 节的结论需修正**：不必新建 lab→provider 别名表，直接透传 `h.get("official")` 即可。当前 `catalog.rs` L1362–1363 的问题是**没有使用该字段**，而非该字段不存在：

```rust
// 现状（错误）：用 ref 渠道反推 official
let official = boolean(h.get("official"))
    || (Some(&p_id) == model.ref_provider.as_ref() && model.ref_official);

// 应改为：直接采用上游判定
let is_official = boolean(h.get("official"));
```

别名表降级为**兜底与校验**手段（当 `official` 全为 false 时用于交叉验证）。

### 修正二：`modelId` 也已提供，无需 models.dev

详情页每条 host 都带 `modelId`（渠道声明的 model id）。实测与 models.dev `providers[].models` 的 key **完全一致**：

| 渠道 | llmpricing 页面 modelId | models.dev key | 价格 |
|---|---|---|---|
| crof | `glm-5.2` | `glm-5.2` | 0.3 / 1.05 ✅ |
| nano-gpt | `z-ai/glm-5.2` | `z-ai/glm-5.2` | 0.42 / 1.32 ✅ |
| scx-ai | `GLM-5.2` | `GLM-5.2` | — |

**因此第 3.5 节"declaredModelId 来自 models.dev"的说法不准确**——llmpricing 详情页同样提供。

### 修正三：真正的冗余是 HTML 爬取，不是 models.dev

当前架构（`catalog.rs` L1107–1131 `fetch_hosts_for_model` + L1138–1200 `prefetch_model_hosts`）通过**爬取 HTML 页面**获取渠道明细：

- 每页约 **245 KB**
- 需爬取约 **1800 个模型**（`hosts_json` 为空且非 deprecated）
- 并发 8（L1548 `prefetch_model_hosts(database, 8)`）
- 合计约 **440 MB HTML**，且无 JSON API（`/api/m/...`、`/m/.../index.json` 等均 404）

而 models.dev `catalog.json` **单个 4.95 MB 文件**即含 7785 条渠道记录（provider + declaredModelId + cost），实测：

| 指标 | 结果 |
|---|---|
| 渠道覆盖（glm-5.2） | 73 / 87 = 84% |
| 价格一致率 | 60 / 64 = 94% |
| 请求数 | 1 次 vs 约 1800 次 |
| 数据量 | 4.95 MB vs 约 440 MB |

**结论**：models.dev 的价值主要在**投递机制**（单文件替代大规模 HTML 爬取），而非字段独有性。

### 修正后的数据源分工

| 数据源 | 不可替代的内容 |
|---|---|
| **llmpricing** | AA 评测分（6 项）、`kind` 分类、`tier`（供应商性质）、`blended*`、`min` 可信最低价、`usage`、**canonical 归并**（1941 个模型的分组）、`official` 标志 |
| **models.dev** | `license`、`links`、`benchmarks` 明细、**单文件替代 HTML 爬取**、修正 175 个 `misc/*` 的 lab |
| **可废弃** | ❌ 不是任一数据源，而是 **HTML 爬取子系统**（`fetch_hosts_for_model` + `prefetch_model_hosts`） |

> 补充证据：渠道声明的 model id 极度混乱，同一 GLM-5.2 有 7 种写法（`glm-5.2` / `z-ai/glm-5.2` / `zai-org/GLM-5.2-TEE` / `databricks-glm-5-2` / `zai-glm-5-2` / `zai-org-glm-5-2` / `glm5.2`）。**这正说明 canonical 归并不可或缺**——而这套归并只有 llmpricing 在做。

---

## 一、结论

**需求可实现，且所需数据已齐备。**


三个关键前提全部验证通过：

| 前提 | 结果 |
|---|---|
| 渠道性质可判定（官网/云/三方） | ✅ llmpricing manifest 的 `tier` 字段覆盖 **213/213 = 100%** |
| 原始 lab + 原始 model id 可得 | ✅ canonical id 天然是 `lab/modelId` 格式，100% 可解析 |
| 官方渠道可识别 | ✅ 头部模型 **66%** 有官方渠道，可用别名表精确判定 |

**且发现一个额外收益**：models.dev 能修正 llmpricing 现有的 lab 误判（175 个 `misc/*` 中可修正 52 个）。

---

## 二、核心设计：三层数据模型

### 第一层　模型身份（Identity）

跨渠道归一化的锚点。回答"这是哪个 lab 的哪个模型"。

| 字段 | 含义 | 示例 |
|---|---|---|
| `officialLab` | 原始实验室 | `zhipuai` |
| `officialModelId` | 原始模型标识（lab 官方声明） | `glm-5.2` |
| `labLabel` | 展示名 | `智谱 AI (GLM)` |
| `identitySource` | 身份来源（便于排查） | `canonical` / `inferred` / `manual` |

**来源**：canonical id 前缀 = lab，后缀 = modelId。这是 models.dev 与 llmpricing 共用的命名空间，100% 可解析。

### 第二层　渠道分层（Host Tiering）

回答"这家渠道是什么性质"。

| 层级 | `tier` | 家数 | 说明 |
|---|---|---|---|
| 官方实验室 | `lab` | 31 | openai / anthropic / google / alibaba / zhipuai / deepseek … |
| 云厂商 | `cloud` | 39 | amazon-bedrock / azure / groq / togetherai / deepinfra … |
| 三方网关 | `gateway` | 143 | openrouter / nano-gpt / aihubmix … |

**⚠️ 关键区分**：`tier` 描述的是**供应商自身性质**，不等于它是该模型的官方渠道。

反例：`nvidia` 是 tier=lab（NVIDIA 自研 Nemotron），但它也转售 GLM-5.2——此时它**不是** GLM 的官方渠道。

所以需要**两个独立字段**：

| 字段 | 判定依据 | 含义 |
|---|---|---|
| `tier` | 供应商自身性质 | 这家渠道是 lab / cloud / gateway |
| `isOfficial` | 供应商 ∈ 该模型原始 lab 的官方渠道集合 | 这是不是该模型的官网渠道 |

### 第三层　价格对比（Price Baseline）

回答"三方比官网贵多少 / 便宜多少"。

| 字段 | 含义 |
|---|---|
| `officialInputCost` / `officialOutputCost` | 官方价基准 |
| `gatewayMinInputCost` / `gatewayMinOutputCost` | 三方渠道最低价 |
| `officialVsGatewayRatio` | 三方相对官方的价差倍数 |
| `hasOfficialHost` | 是否有官方渠道（决定能否做对比） |

---

## 三、数据来源与覆盖率

### 3.1 渠道分层分布（全量 1941 模型）

| 分类 | 数量 | 占比 |
|---|---|---|
| 仅三方网关 | 1017 | 52% |
| 仅云厂商 | 501 | 25% |
| **有官方渠道** | **393** | **20%** |
| 无渠道数据 | 30 | 1% |

### 3.2 官方渠道识别率：头部 vs 长尾

| 分组 | 有官方渠道 | 占比 |
|---|---|---|
| models.dev canonical 头部（382） | 255 | **66%** |
| llmpricing 独有长尾（1559） | 138 | 8% |

**这是符合业务逻辑的**：头部旗舰模型有官方 API，长尾多为开源权重模型、旧版本或路由模型，本就只通过三方提供。

### 3.3 各主要 lab 的官方渠道覆盖

| lab | 覆盖 | lab | 覆盖 |
|---|---|---|---|
| amazon | 19/33 = 57% | google | 39/169 = 23% |
| nvidia | 40/73 = 54% | openai | 48/244 = 19% |
| alibaba | 69/194 = 35% | zhipuai | 16/98 = 16% |
| mistral | 32/108 = 29% | moonshotai | 4/40 = 10% |
| xai | 12/47 = 25% | anthropic | 14/162 = 8% |
| minimax | 7/35 = 20% | deepseek | 4/61 = 6% |

> 覆盖率低不代表识别失败——多数是旧版本模型（如 `claude-3-*`）已从官方 API 下架。

### 3.4 lab → 官方渠道别名表（初版 24 条）

这是识别 `isOfficial` 的核心配置，需人工维护：

```rust
// lab -> 官方渠道 provider id 集合
const LAB_OFFICIAL_HOSTS: &[(&str, &[&str])] = &[
    ("openai",       &["openai"]),
    ("anthropic",    &["anthropic"]),
    ("google",       &["google", "google-vertex"]),
    ("alibaba",      &["alibaba", "alibaba-cn"]),
    ("zhipuai",      &["zhipuai", "zai"]),          // zai = Z.ai 国际品牌
    ("deepseek",     &["deepseek"]),
    ("minimax",      &["minimax", "minimax-cn"]),
    ("moonshotai",   &["moonshotai", "moonshotai-cn"]),
    ("stepfun",      &["stepfun", "stepfun-ai"]),
    ("meta",         &["meta", "llama"]),
    ("microsoft",    &["azure", "azure-cognitive-services"]),
    ("amazon",       &["amazon-bedrock"]),
    ("tencent",      &["tencent-tokenhub"]),
    ("xai",          &["xai"]),
    ("mistral",      &["mistral"]),
    ("cohere",       &["cohere"]),
    ("nvidia",       &["nvidia"]),
    ("xiaomi",       &["xiaomi"]),
    ("perplexity",   &["perplexity"]),
    ("poolside",     &["poolside"]),
    ("sakana",       &["sakana"]),
    ("sarvam",       &["sarvam"]),
    ("longcat",      &["longcat"]),
    ("thinkingmachines", &["thinkingmachines"]),
];
```

**说明**：
- `zai` 是智谱的海外品牌（Z.ai），与 `zhipuai` 同源
- `azure` 归入 `microsoft`，因 Azure 是微软官方云；但 Azure 同时也是 OpenAI 模型的官方渠道，可按需扩展
- 不在表内的 lab 走默认规则：`lab` 自身即官方渠道 id

### 3.5 渠道声明的 model id

`declaredModelId` = 该渠道 API 实际调用的模型名，用于**模型请求分析**（把日志里的模型名归一化到 `officialModelId`）。

**来源**：models.dev `providers[].models` 的 key，定位率 **356/382 = 93%**。

---

## 四、需要修正的现有问题

### 4.1 `official` 字段语义错误（必须改）

当前实现（`catalog.rs` L1362–1363）：

```rust
let official = boolean(h.get("official"))
    || (Some(&p_id) == model.ref_provider.as_ref() && model.ref_official);
```

**问题**：这判定的是"该渠道是否为参考价渠道"，而不是"是否为该模型的原始 lab 官方渠道"。

后果：`zhipuai/glm-5.2` 的 `ref` 取自 `alibaba-cn`（国内站），于是 `alibaba-cn` 被标为 `official`——**但阿里不是 GLM 的官方渠道**。

**修正**：

```rust
let is_official = official_hosts_of(&model.official_lab).contains(&p_id);
```

### 4.2 llmpricing 的 lab 误判（建议修正）

llmpricing 有 **175 个 `misc/*`** 模型，lab 无法识别。实测其中藏有真实归属：

| 误判 id | 真实归属 |
|---|---|
| `misc/step-1-32k`、`misc/step-2-16k` | `stepfun`（阶跃星辰） |
| `misc/doubao-seed-2-0-code-preview-260215` | `bytedance`（字节豆包） |
| `misc/wan2.7-image`、`misc/happyhorse-1.1-*` | `alibaba`（通义万相） |
| `misc/tongyi-intent-detect-v3` | `alibaba`（通义） |
| `misc/ministral-14b-2512` | `mistral` |
| `misc/auto`、`misc/free`、`misc/model-router` | 无 lab（路由模型，合理） |

**修正手段与效果**：

| 手段 | 可修正数 |
|---|---|
| models.dev provider 层 tier=lab 线索 | 4 |
| 模型名关键词规则 | 48 |
| **合计自动修正** | **52 / 175 = 30%** |
| 需人工维护 | 123 |

**关键词规则初版**：

```rust
const LAB_KEYWORDS: &[(&str, &str)] = &[
    ("doubao", "bytedance"), ("seed", "bytedance"),
    ("wan2", "alibaba"), ("wanx", "alibaba"), ("qwen", "alibaba"),
    ("tongyi", "alibaba"), ("happyhorse", "alibaba"),
    ("step", "stepfun"), ("glm", "zhipuai"), ("kimi", "moonshotai"),
    ("minimax", "minimax"), ("hunyuan", "tencent"), ("ernie", "baidu"),
    ("llama", "meta"), ("gemma", "google"), ("grok", "xai"),
    ("mistral", "mistral"), ("ministral", "mistral"),
    ("nemotron", "nvidia"), ("flux", "black-forest-labs"),
];
```

> ⚠️ `seed` 关键词有误伤风险（如 `seedream` 是字节的，但 `seed` 也可能出现在其他名字里），建议只对 `misc/*` 生效。

---

## 五、表结构改造

### 5.1 `model_catalog_models` 新增列

```sql
-- 模型身份
official_lab         TEXT NOT NULL DEFAULT '',   -- 原始 lab
official_model_id    TEXT NOT NULL DEFAULT '',   -- 原始 model id
lab_label            TEXT NOT NULL DEFAULT '',   -- 展示名
identity_source      TEXT NOT NULL DEFAULT '',   -- canonical/inferred/manual

-- 官方渠道与价格基准
has_official_host    INTEGER NOT NULL DEFAULT 0,
official_host_count  INTEGER NOT NULL DEFAULT 0,
official_input_cost  REAL,
official_output_cost REAL,

-- 渠道分层统计
lab_host_count       INTEGER NOT NULL DEFAULT 0,
cloud_host_count     INTEGER NOT NULL DEFAULT 0,
gateway_host_count   INTEGER NOT NULL DEFAULT 0,

-- 价格对比
gateway_min_input_cost  REAL,
gateway_min_output_cost REAL,
official_vs_gateway_ratio REAL;

CREATE INDEX IF NOT EXISTS idx_mcm_official_lab ON model_catalog_models(official_lab);
CREATE INDEX IF NOT EXISTS idx_mcm_has_official ON model_catalog_models(has_official_host);
```

### 5.2 `model_catalog_providers` 新增列

```sql
tier TEXT NOT NULL DEFAULT 'gateway';   -- lab / cloud / gateway
is_first_party INTEGER NOT NULL DEFAULT 0;  -- tier == 'lab'
```

> `tier` 列已存在（`catalog.rs` L294），只需补 `is_first_party`。

### 5.3 `model_catalog_hosts`（若已落库）

```sql
tier            TEXT NOT NULL DEFAULT 'gateway',
is_official     INTEGER NOT NULL DEFAULT 0,  -- 修正语义
declared_model_id TEXT,                        -- 该渠道声明的 model id
official_model_id TEXT;                        -- 归一化后的原始 model id
```

---

## 六、代码改造点

| 位置 | 改动 |
|---|---|
| `catalog.rs` L13–15 | 新增 `MODELS_DEV_CATALOG_URL`；`CATALOG_SCHEMA_VERSION` `"9"` → `"10"` |
| `catalog.rs` L30–42 | `ModelCatalogProvider` 增加 `is_first_party: bool` |
| `catalog.rs` L46–64 | `ModelCatalogHostItem` 增加 `is_official`（语义修正）、`declared_model_id`、`official_model_id` |
| `catalog.rs` L77–130 | `ModelCatalogItem` 增加 13 个新字段（见 5.1） |
| `catalog.rs` L294 | `model_catalog_providers` 表补 `is_first_party` |
| `catalog.rs` L300–355 | `model_catalog_models` 表补 13 列 + 2 索引 |
| `catalog.rs` L404–534 | `parse_model_item_from_json` 增加身份解析（拆 id 前缀/后缀） |
| `catalog.rs` L1331–1397 | **`official` 判定逻辑重写**（见 4.1）；补 `declared_model_id` |
| 新增模块 | `lab_registry.rs`：别名表 + 关键词规则 + 官方渠道判定 |
| `types.ts` L908–1018 | 同步新增字段 |

### 核心函数签名（新增）

```rust
/// 解析模型身份：canonical id -> (lab, model_id)
fn parse_identity(canonical_id: &str) -> (String, String);

/// 取某 lab 的全部官方渠道 provider id
fn official_hosts_of(lab: &str) -> Vec<&'static str>;

/// 判定渠道是否为该模型的官方渠道
fn is_official_host(lab: &str, provider_id: &str) -> bool;

/// 修正 misc/* 的 lab 归属
fn infer_lab(name: &str, host_providers: &[String]) -> Option<String>;

/// 渠道分层统计
fn tier_counts(host_providers: &[String], tier_map: &HashMap<String, String>)
    -> (usize, usize, usize);  // (lab, cloud, gateway)
```

---

## 七、两个目标场景的落地

### 7.1 价格对比

有了官方基准后，可以表达：

```
GLM-5.2（原始 lab: zhipuai，原始 model id: glm-5.2）
├─ 官网价       $1.4 / $4.4      ← 官方渠道 zhipuai / zai
├─ 三方最低     $0.30 / $1.05    ← crof
└─ 最大价差     7.7 倍
```

**可用筛选项**：
- 「仅看有官方渠道的模型」（`has_official_host = 1`，393 款）
- 「三方折扣 > 50%」
- 「官方渠道 vs 三方渠道」价格并列展示

**注意**：`min` 最低价来自 llmpricing（含过滤规则，排除免费渠道），**不要**用 models.dev 原始最低值替换，否则会出现 $0 的免费渠道污染对比。

### 7.2 模型请求分析

请求日志里的模型名是**渠道声明名**，格式五花八门：

| 日志中的模型名 | 渠道 | 归一化后 |
|---|---|---|
| `glm-5.2` | zhipuai（官网） | `zhipuai/glm-5.2` |
| `zai/glm-5.2` | zai | `zhipuai/glm-5.2` |
| `zhipuai/glm-5.2` | openrouter | `zhipuai/glm-5.2` |
| `glm-5.2` | nano-gpt | `zhipuai/glm-5.2` |

**归一化流程**：

```
日志模型名 + 渠道 id
  → 查 provider_index[(provider, declared_model_id)]
  → 命中则取 officialModelId
  → 未命中则按 bare id / 名称关键词兜底
  → 输出 canonical id 作为统一分析维度
```

这样才能统计"GLM-5.2 在哪些渠道被调用了多少次"，而不是被 4 种写法拆成 4 个模型。

---

## 八、风险与验证清单

| 风险 | 等级 | 缓解 |
|---|---|---|
| `official` 语义改错导致 UI 行为变化 | 🔴 高 | 保留旧字段 `is_ref`，新增 `is_official`，UI 迁移期并存 |
| 别名表维护滞后（新 lab 上线） | 🟠 中 | 默认规则兜底（lab 自身即官方 id）；加未命中告警 |
| `misc/*` 123 个无法自动归属 | 🟠 中 | 保留 `misc` 并标 `identitySource = unknown`，UI 不隐藏 |
| 关键词规则误伤 | 🟠 中 | 仅对 `misc/*` 生效；`seed` 等宽泛词需加白名单 |
| 长尾 92% 无官方渠道 | 🟡 低 | 属客观事实；UI 显示「仅三方提供」而非留空 |
| 云厂商归属争议（Azure vs Microsoft） | 🟡 低 | 在别名表中显式声明，不靠推断 |

### 上线前验证

- [ ] `tier` 三层计数之和 == `hostCount`
- [ ] 393 款有官方渠道的模型，`is_official` 至少命中 1 家
- [ ] `zhipuai/glm-5.2` 的官方渠道为 `zhipuai` + `zai`，**不含** `alibaba-cn`
- [ ] `openai/gpt-5.5` 的官方渠道含 `openai`
- [ ] `misc/step-1-32k` 的 lab 修正为 `stepfun`
- [ ] `misc/auto` 保持无 lab（路由模型）
- [ ] `declaredModelId` 定位率 ≥ 90%
- [ ] 价格对比页：官方基准 ≠ 三方最低（不应出现 $0 污染）
- [ ] 请求分析：同一模型的多种日志写法能归一化到同一 canonical id

---

## 九、一句话总结

**需求可实现。** `tier` 字段（lab/cloud/gateway，覆盖 100%）解决渠道性质分类，canonical id 前缀/后缀解决原始 lab 与 model id 还原，别名表解决官方渠道判定。**关键是要把 `official` 从"参考价渠道"改为"模型原始 lab 渠道"**——现有实现会把 `alibaba-cn` 误判为 GLM-5.2 的官方渠道。头部模型 66% 可做官方/三方价格对比，长尾 92% 无官方渠道属客观事实。

---

## 十、免费渠道识别：能否脱离 HTML 爬取

爬取的核心目的是**识别哪些渠道免费提供模型**。本节验证能否用静态 JSON 替代。

### 10.1 llmpricing 已有的免费数据

`rows` 分片中已有三个计数字段（但只有数量，无具体渠道）：

| 字段 | 含义 |
|---|---|
| `freeHostCount` | 免费渠道数 |
| `subHostCount` | 订阅制渠道数 |
| `pricedHostCount` | 付费渠道数 |

全量统计：**330 / 1941（17%）** 的模型有免费渠道，免费渠道条目共 **440 条**。

判定规则（App 代码 `catalog.rs` L1364）：

```rust
let is_free = (input == Some(0.0) && output == Some(0.0)) && !subscription;
```

`subscription` 来自 manifest，共 **22 家**订阅制供应商：`alibaba-token-plan`、`zai-coding-plan`、`github-copilot`、`tencent-token-plan`、`xiaomi-token-plan-*`、`minimax-coding-plan` 等。

### 10.2 三种方案实测对比

以 llmpricing 的 `freeHostCount` 为基准：

| 方案 | 一致率 | 请求数 | 体积 | 解析方式 |
|---|---|---|---|---|
| 现状 HTML 爬取 | 100% | 约 1800 | 约 440 MB | HTML 正则 |
| **组合：`hostProviders` + models.dev 价格** | **96%（1869/1941）** | **7** | **约 6.6 MB** | **JSON** |
| 纯 models.dev 推算 | 94%（1829/1941） | 1 | 4.95 MB | JSON |

**组合方案**：用 llmpricing 的 `hostProviders`（已完成 canonical 归并的渠道列表）圈定范围，再用 models.dev 的 `cost` 判定哪家为 0。

### 10.3 关键结论：差异是命名变体，不是数据缺失

实测发现 **models.dev 其实拥有全部免费渠道**，只是拼写不同：

| 渠道 | llmpricing 拼写 | models.dev 拼写 |
|---|---|---|
| kenari | `glm-5-2` | `glm-5-2`（点号→连字符） |
| unorouter | `glm-5.2` | `glm-5.2:free`（带变体后缀） |
| vercel | `llama-3.3-70b-instruct` | `meta/llama-3.3-70b`（缺 `-instruct`） |
| kenari | `gemini-3.1-pro-preview` | `gemini-3-1-pro`（缺 `-preview`） |
| pendra | `llama-3.3-70b` | `llama3.3:70b`（`llama3.3` vs `llama-3.3`） |

**归一化函数**（已验证可达 96%）：

```rust
fn normalize_model_id(s: &str) -> String {
    let mut s = s.to_lowercase();
    s = s.split([':', '@']).next().unwrap_or("").to_string();  // 去 :free :thinking
    s = s.rsplit('/').next().unwrap_or("").to_string();        // 去 lab/provider 前缀
    s = s.replace(['.', '_'], "-");
    // 字母数字边界插连字符：llama3 -> llama-3
    // 循环剥离后缀：-instruct / -preview / -latest / -chat / -it / -turbo
    //              / -thinking / -exp / -fp8 / -tee / -free
    s
}
```

> ⚠️ **不要过度剥离**：若无条件剥离 `-free`，`zenmux` 会被误判为 GLM-5.2 的免费渠道，引入假阳性。**96% 是稳定点**。

### 10.4 意外发现：models.dev 有时更全

`deepseek/deepseek-v3.2` 案例中，models.dev 多找到一个免费渠道 `iflowcn`，而 llmpricing 的 `freeHostCount` 为 0。

**说明 models.dev 在部分场景比 llmpricing 更完整**，两者并非简单的包含关系。

### 10.5 建议

**用组合方案替代 HTML 爬取**，并在 UI 上对免费标记保留"以渠道实际为准"的提示。若个别模型的免费状态对用户决策至关重要，可保留按需单模型校验的兜底路径。

---

## 附录：验证数据

| 指标 | 数值 |
|---|---|
| `tier` 覆盖 | 213 / 213 = 100% |
| tier 分布 | lab 31 / cloud 39 / gateway 143 |
| 有官方渠道的模型 | 393 / 1941 = 20% |
| 头部官方渠道识别率 | 255 / 382 = 66% |
| 长尾官方渠道识别率 | 138 / 1559 = 8% |
| `declaredModelId` 定位率 | 356 / 382 = 93% |
| llmpricing `misc/*` 误判 | 175 个，可自动修正 52 |
| lab 前缀 == lab 字段（llmpricing 内部） | 1941 / 1941 = 100% |

代码依据：
- `src-tauri/src/model/catalog/catalog.rs` L30–64（类型）、L294–355（表结构）、L404–534（解析）、L1331–1397（**渠道构建与 official 判定，重点改造**）
- `src/types.ts` L908–1018
