# Token 采集器 Bug 修复总结

## 已修复的关键 Bug (2024-09-03)

### P0 级别 - 数据损坏修复

#### 1. CatPawAI 时间戳乘数错误 ✅ 已修复
**文件:** `src-tauri/src/token/collector/sources/catpawai.rs`

**问题:** 
- 时间戳判定阈值使用 `100_000_000_000`（10^11），对应 2073 年
- 导致所有 2001-2073 年的数据被错误地乘以 1000
- 结果：会话显示在 5000 年而不是真实日期

**修复:**
- 将阈值改为 `10_000_000_000`（10^10）
- 10^10 毫秒 = 2001-09-09，10^10 秒 = 2286 年
- 修复了 3 处时间戳转换位置：
  - `t_conversations` 表的 create_time/update_time (行 207-216)
  - `t_conversation` 表的 created_at/ts (行 258-264)
  - `t_ui_messages` 表的 create_time (行 317-321)

**影响:** 修复后所有 CatPawAI 会话时间戳将正确显示在 2020-2026 范围内

---

#### 2. Cursor 估算标记混淆 ✅ 已修复
**文件:** `src-tauri/src/token/collector/sources/cursor.rs`

**问题:**
- 使用 `estimated = cached == 0` 判断是否为估算
- 真实的零缓存命中事件（cached 确实为 0）被错误标记为估算
- 前端排除估算事件，导致这些准确事件从统计中消失

**修复:**
- 改为检查是否有 input/output 明细字段
- 只有完全缺失 token 明细时才标记为估算
- 修复了 2 处：
  - AI 响应处理 (行 173-202)
  - aiService.prompts 处理 (行 273-292)

```rust
// 修复前
let estimated = cached == 0;

// 修复后
let has_breakdown = bubble.get("inputTokens").is_some()
    || bubble.get("promptTokens").is_some()
    || bubble.get("outputTokens").is_some()
    || bubble.get("completionTokens").is_some();
let estimated = !has_breakdown;
```

**影响:** 缓存命中率统计将包含所有有效数据，不再排除真实的零缓存会话

---

### P1 级别 - 数据丢失修复

#### 3. Windsurf 缺失时间边界 ✅ 已修复
**文件:** `src-tauri/src/token/collector/sources/windsurf.rs`

**问题:**
- `first_ts` 和 `last_ts` 声明为不可变（`let`）
- 无法更新时间边界，所有会话显示空时间范围
- 时间序列分析不可用

**修复:**
- 改为可变变量（`let mut`）
- 从事件中提取时间戳（支持多种字段名）
- 每个事件更新会话时间边界

**新增功能:**
- 支持 `timestamp`、`createdAt`、`created_at` 字段
- 支持毫秒数值和 ISO 字符串格式
- 自动更新 `first_ts` 和 `last_ts`

**影响:** Windsurf 会话现在有正确的开始/结束时间，支持时间序列分析

---

### P2 级别 - 估算精度修复

#### 4. Goose 降级协议改进 ✅ 已修复
**文件:** `src-tauri/src/token/collector/sources/goose.rs`

**问题:**
- 只有 `total_tokens` 时，全部归入 `input_tokens`，`output_tokens = 0`
- 统计上不可能（100% 输入，0% 输出）
- 成本计算错误（忽略输出定价）

**修复:**
- 根据消息角色智能分配：
  - `user` 消息：全部归入输入（合理）
  - `assistant` 消息：按 2:1 比例分配输入/输出
    - 输入 ≈ 2/3 (上下文)
    - 输出 ≈ 1/3 (响应)

**影响:** 降级模式下的统计更接近实际，成本估算更准确

---

#### 5. Goose Reasoning Tokens 聚合 ✅ 已修复
**文件:** `src-tauri/src/token/collector/sources/goose.rs`

**问题:**
- 事件级别记录了 `reasoning_output_tokens`
- 会话级别硬编码为 0，聚合时丢失

**修复:**
- 添加 `total_reasoning` 累加器
- 在会话汇总中使用实际累加值
- 与其他 token 类型保持一致

**影响:** Reasoning token 使用量现在正确显示在会话和项目汇总中

---

## 修复统计

- **文件修改:** 4 个源文件
- **代码行数:** ~100 行变更
- **Bug 修复:** 5 个关键 bug
- **优先级分布:**
  - P0 (数据损坏): 2 个
  - P1 (数据丢失): 1 个  
  - P2 (精度问题): 2 个

## 测试建议

1. **CatPawAI 时间戳验证:**
   - 检查所有会话时间在 2020-2026 范围内
   - 不应有 1970 或 5000 年的异常数据

2. **Cursor 缓存统计验证:**
   - 检查缓存命中率是否包含零缓存会话
   - 对比修复前后的缓存命中率变化

3. **Windsurf 时间范围验证:**
   - 检查所有会话是否有 started_at/ended_at
   - 验证时间顺序合理性

4. **Goose 降级模式验证:**
   - 检查只有 total_tokens 的事件
   - 验证 input/output 比例合理（不再是 100%/0%）

5. **Reasoning Tokens 验证:**
   - 检查会话级别 reasoning_output_tokens > 0
   - 验证与事件级别的总和一致

## 待修复的 Bug

根据 `/Users/wusuoming/.claude/plans/agent-token-bug-sparkling-ullman.md` 分析报告，还有以下高优先级 bug 待修复：

### P0 剩余
- Codex 双协议误判（44% token 虚增）
- ZCode 供应商过滤（静默数据丢失）

### P1 剩余
- SQLite 数据库锁定处理
- Copilot transcript 前向兼容性
- DSH 去重竞态条件

### P2 剩余
- Antigravity 上下文累积（10x-100x 虚增）
- Copilot CLI 上下文累积
- Command Code 中日韩字符估算

详见完整分析报告以了解所有 40+ bug 的详细信息。

## 回归风险

所有修复均为**向后兼容**：
- 不改变现有正确数据的解析结果
- 仅修正错误情况的处理逻辑
- 不引入新的依赖或 API 变更

建议在测试环境验证后再部署到生产环境。
