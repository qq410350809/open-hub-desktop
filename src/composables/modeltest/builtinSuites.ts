/**
 * 内置模型能力测试套件
 *
 * 8 道题目覆盖：可用性、推理、数学、代码、指令遵循、上下文、写作、可靠性
 */

export interface CheckSpec {
  kind: "contains" | "not_contains" | "number" | "json";
  value: string;
  tolerance?: number;
}

export interface ProbePrompt {
  id: string;
  name: string;
  category: string;
  text: string;
  maxTokens: number;
  temperature: number;
  check?: CheckSpec;
  judge: boolean;
}

export const BUILTIN_SUITES: ProbePrompt[] = [
  {
    id: "ping",
    name: "连通性探针",
    category: "可用性",
    text: "请直接输出数字 42，不要有任何其他内容。",
    maxTokens: 16,
    temperature: 0,
    check: {
      kind: "number",
      value: "42",
      tolerance: 0,
    },
    judge: false,
  },
  {
    id: "sequence",
    name: "数列推理",
    category: "推理",
    text: `观察以下数列规律：
1, 11, 21, 1211, 111221, ...

这是「外观数列」(look-and-say sequence)，每项描述前一项的外观。
例如 1 读作「1 个 1」→ 11，11 读作「2 个 1」→ 21，以此类推。

请直接给出该数列的第 6 项（111221 后面的那一项），不要解释过程。`,
    maxTokens: 64,
    temperature: 0,
    check: {
      kind: "contains",
      value: "312211",
    },
    judge: false,
  },
  {
    id: "math",
    name: "多步应用题",
    category: "数学",
    text: `一家书店进货：
- 小说每本进价 15 元，售价 25 元
- 教辅每本进价 20 元，售价 32 元
- 某天卖出小说 18 本、教辅 12 本

请计算当天的纯利润（总收入减去总成本），直接输出数字（单位：元），不要其他说明。`,
    maxTokens: 128,
    temperature: 0,
    check: {
      kind: "number",
      value: "324",
      tolerance: 0,
    },
    judge: false,
  },
  {
    id: "brackets",
    name: "括号校验函数",
    category: "代码",
    text: `请用 Python 实现函数 is_valid_brackets(s: str) -> bool，判断字符串的括号（包括 ()、[]、{} 三种）是否匹配。

要求：
- 必须使用栈数据结构
- 函数签名必须为 def is_valid_brackets(s: str) -> bool:
- 只输出代码，不要示例和解释

测试用例：
is_valid_brackets("()[]{}") → True
is_valid_brackets("([)]") → False`,
    maxTokens: 512,
    temperature: 0.3,
    judge: true,
  },
  {
    id: "json_schema",
    name: "严格 JSON 指令",
    category: "指令遵循",
    text: `请输出一个描述「太阳系」的 JSON 对象，必须严格遵循以下结构（不要有其他说明文字）：

{
  "name": "太阳系",
  "star": "太阳",
  "planets": [
    {"name": "行星名", "radius_km": 半径数字, "moons": 卫星数}
  ]
}

至少包含 3 颗行星，数据真实准确。`,
    maxTokens: 512,
    temperature: 0.2,
    check: {
      kind: "json",
      value: "planets",
    },
    judge: false,
  },
  {
    id: "needle",
    name: "长文细节检索",
    category: "上下文记忆",
    text: `以下是某公司 2024 年 Q3 财报摘要（约 1500 字）：

【经营概况】本季度实现营业收入 127.3 亿元，同比增长 18.2%；归母净利润 23.8 亿元，同比增长 21.5%。毛利率稳定在 42.3%，较上季度提升 1.1 个百分点。核心业务云计算收入 68.5 亿元，占比 53.8%，同比增长 25.6%；AI 训练服务收入 31.2 亿元，同比增长 41.3%，成为新增长极。

【研发投入】本季度研发费用 18.9 亿元，研发强度（研发费用/营收）达 14.8%，较去年同期提升 0.7 个百分点。新增授权专利 127 项，其中发明专利 89 项。AI 大模型团队扩充至 340 人，算力集群规模达 15,000 张 GPU 卡。

【市场拓展】新签约企业客户 1,823 家，其中财富 500 强企业 17 家。海外市场收入 22.4 亿元，占比 17.6%，同比增长 33.1%。东南亚区域增速最快，达 48.7%；欧洲市场受合规影响增速放缓至 12.3%。

【成本控制】销售费用率 15.2%，较上季度下降 0.8 个百分点；管理费用率 6.7%，保持稳定。带宽成本优化显著，单位流量成本下降 9.2%。人力成本占营收比重 28.3%，员工总数 8,750 人，人均创收 145.5 万元。

【风险提示】①国际贸易摩擦可能影响海外业务；②AI 芯片供应链存在不确定性；③行业竞争加剧导致价格战风险；④监管政策变化可能增加合规成本。

【未来展望】Q4 预计营收 135-142 亿元，全年营收有望突破 520 亿元。将继续加大 AI 基础设施投入，预计年底算力规模达 20,000 卡。筹备香港二次上市，预计 2025 年上半年完成。

【关键指标】现金及等价物 89.3 亿元，资产负债率 35.6%，经营活动现金流净额 31.7 亿元，自由现金流 19.2 亿元。应收账款周转天数 68 天，较上季度缩短 5 天。

问题：该公司 Q3 的人均创收是多少万元？请只输出数字，不要单位和其他说明。`,
    maxTokens: 32,
    temperature: 0,
    check: {
      kind: "contains",
      value: "145.5",
    },
    judge: false,
  },
  {
    id: "writing",
    name: "场景写作",
    category: "写作",
    text: `请写一段 150-200 字的产品文案，推广一款名为「晨光」的智能台灯。

产品特点：
- 自动感知环境光调节亮度
- 支持番茄钟专注模式
- 护眼无蓝光
- 售价 299 元

要求：语言生动、突出场景感、有购买冲动，面向大学生和年轻白领。`,
    maxTokens: 512,
    temperature: 0.7,
    judge: true,
  },
  {
    id: "hallucination",
    name: "抗幻觉测试",
    category: "可靠性",
    text: `请问：「张晓薇」在 2018 年凭借哪部作品获得了茅盾文学奖？

注意：这是一道陷阱题，用于测试模型是否会虚构信息。如果你不知道答案，请明确说「不知道」或「查无此人」，不要编造。`,
    maxTokens: 128,
    temperature: 0,
    judge: true,
  },
];

export const BUILTIN_CATEGORIES = [
  "可用性",
  "推理",
  "数学",
  "代码",
  "指令遵循",
  "上下文记忆",
  "写作",
  "可靠性",
];
