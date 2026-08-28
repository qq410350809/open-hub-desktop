# OpenHub

**AI 公益站资产管理工作台** —— 一个使用 Tauri 2 + Vue 3 + Rust + SQLite 构建的本地优先应用：把分散的 AI 公益站/中转站集中管理起来，通过 Chrome 会话同步账号、通过代理池保障连通性、通过内置模型网关聚合多站点 API，并完整统计每一分 Token 的去向。

同一套业务内核支持两种交付形态：

- **一体式桌面客户端**：常规桌面应用，本地 SQLite 存储 + 内嵌 HTTP 服务；
- **独立服务端**（`openhub-server`）：无窗口依赖的单文件二进制，可部署到本机、NAS 或 VPS，通过浏览器访问 Web UI。

瘦客户端的双通道运行时已在前端协议层就绪（远程业务走 HTTP RPC/SSE，本地 Token 统计走客户端 IPC）；独立瘦客户端壳属于后续发布阶段。

---

## 目录

- [核心特性](#核心特性)
- [设计理念](#设计理念)
- [整体架构](#整体架构)
- [功能模块全景](#功能模块全景)
- [技术选型](#技术选型)
- [代码导览](#代码导览)
- [开发指南](#开发指南)
- [构建与打包](#构建与打包)
- [部署指南](#部署指南)
- [Web API 参考](#web-api-参考)
- [数据库设计](#数据库设计)
- [测试与质量](#测试与质量)
- [CI/CD 发布流程](#cicd-发布流程)
- [安全模型](#安全模型)
- [故障排除](#故障排除)

---

## 核心特性

| 能力 | 说明 |
|------|------|
| 🗂️ 站点资料库 | 管理 AI 公益站的名称、API 地址、系统类型、标签、维护者、签到福利、限额等 30+ 维度信息，内置种子数据 |
| 👥 多账号同步 | 自动检测本机 Chrome Profile 会话，批量同步站点账号状态、余额与用量 |
| 🌐 代理池 | 订阅解析、节点测速、GeoIP 地域分组、多通道管理；Mihomo 内核按需下载 |
| 🧭 模型目录 | 聚合各站点可用模型，支持厂商/能力/价格多维筛选，每日自动增量同步 |
| 🔀 模型网关 | OpenAI / Anthropic / Gemini 三协议兼容的反向代理，多通道聚合、负载均衡、健康检查、请求日志 |
| 📊 Token 统计 | 双数据平面：本地终端日志采集（18 种 AI 工具）+ 反代网关用量明细与日/小时聚合 |
| 📡 公益监控 | RSS 订阅源聚合、已读跟踪、跑路/假公益标记联动、代理池状态联动 |
| 💻 双形态部署 | 桌面客户端开箱即用；单文件服务端一行命令部署到任意 Linux/macOS/Windows 主机 |

---

## 设计理念

### 1. 双形态同核（One Core, Two Shells)

桌面壳与服务端共享同一个 Rust 业务内核（`open_hub_desktop_lib`），通过 Cargo feature 切换装配面：

```
[features]
default = ["desktop"]   # Tauri 桌面壳 + 内嵌 HTTP 服务
server = []             # 无 Tauri 依赖的独立 HTTP 服务（openhub-server）
```

差异只在最外层：

| | 一体式桌面客户端 | 独立服务端 |
|---|---|---|
| 二进制 | `open-hub-desktop` | `openhub-server` |
| 窗口/WebView | 有（Tauri 2） | 无 |
| 状态托管 | Tauri TypeMap（`Managed`） | 进程内 `LocalRef` |
| 本机能力（Chrome 同步、本地 Token 日志） | 可用 | 按 `/api/caps` 协商关闭 |
| 数据目录 | 平台应用数据目录 | `--data-dir` 指定 |

### 2. 两套互不合并的数据平面

Token 统计刻意拆分为两个来源，避免口径混淆：

- **本地终端 Token**：读取当前设备上 Claude Code、Codex CLI、Cursor 等 AI 工具的本地日志，仅在客户端本地展示，**永不上传**到 Web 服务；
- **反代网关 Token**：只统计经过 OpenHub 模型网关的请求，由服务端保存，桌面端/瘦客户端/浏览器均可查看。

两者共用展示组件，但不共用采集任务，不默认相加，也不把同一终端经网关转发产生的两份记录强行去重。

### 3. 双通道运行时（协议层就绪）

前端通过运行面自动选择调用通道：

```typescript
// integrated → 本地 Tauri IPC
// thin / web → 同源 HTTP RPC（POST /api/rpc）
export async function runCommand<T>(command: string, args) {
  if (clientMode === "integrated") return runLocalCommand<T>(command, args);
  return runServerCommand<T>(command, args);
}
```

- 命令名在 IPC 与 HTTP RPC 之间完全一致，RPC 分发复用桌面端同一套命令实现；
- Rust `Result` 在 RPC 层序列化为 `{"Ok":..}/{"Err":..}`，前端统一解包对齐桌面 invoke 语义；
- 事件同理：集成端走 Tauri event，瘦客户端/浏览器走 SSE（`GET /api/events`），自动重连。

### 4. 能力协商而非能力假设

客户端启动时通过 `/api/caps` 合并本地与服务端能力：

- 浏览器只接受服务端能力，不会把服务主机的本地文件能力当成自己的；
- 瘦客户端保留本地 Token 能力，不被远程 `false` 覆盖；
- Chrome 会话同步、本地 Token 日志等本机能力**永远不会**通过 `/api/rpc` 暴露给远端访问者。

### 5. 重组件按需下载

Mihomo 代理内核与 GeoIP 数据库不随安装包分发（减小体积、规避平台分发限制）。首次打开时由前端「组件初始化引导」显式触发下载，保存到应用数据目录的 `bin/` 与 `Country.mmdb`。无网络时主界面照常可用，仅代理池相关功能显示待初始化。

---

## 整体架构

```
┌────────────────────────────────────────────────────────────────────┐
│                        一体式桌面客户端                              │
│                                                                    │
│  Vue 3 UI ──▶ Composables ──▶ runCommand()                         │
│                  │                     │                           │
│                  │ 本地 Token          │ 其余业务                    │
│                  ▼                     ▼                           │
│            Tauri IPC ────────▶ 业务内核（Rust）◀──── 同源 HTTP RPC    │
│                                    │                               │
│      ┌──────────┬──────────┬──────┴─────┬───────────┬──────────┐   │
│      │ site     │ proxypool│ model      │ token     │ charity  │   │
│      │ library  │ (Mihomo) │ catalog+gw │ collector │ monitor  │   │
│      └──────────┴──────────┴──────┬─────┴───────────┴──────────┘   │
│                                   ▼                                │
│                          SQLite（WAL 模式）                         │
│                                   │                                │
│              内嵌 Axum HTTP 服务 :17896（Web UI + API + /v1 网关）   │
└────────────────────────────────────────────────────────────────────┘

┌──────────────────────┐   HTTP RPC + SSE    ┌──────────────────────┐
│  瘦客户端（双通道）    │ ──────────────────▶ │  openhub-server       │
│  本地 Token → IPC     │ ◀───────────────── │  Web UI + 反代统计     │
│  远程业务 → HTTP      │    EventBus 广播    │  站点/代理/模型网关     │
└──────────────────────┘                     └──────────────────────┘

独立浏览器直接访问 openhub-server 时，只使用右侧服务端能力。
```

### 关键链路

| 链路 | 路径 |
|------|------|
| 页面读数据 | Component → Composable → `runCommand` → IPC 或 `/api/rpc` → 命令表分发 → SQLite |
| 后端推送 | Rust `EventBus.emit` → Tauri event（桌面）或 SSE `/api/events`（远程）→ 前端 `listen()` |
| 模型网关请求 | 客户端 → `/v1/chat/completions` 等入口 → API Key 校验 → 通道路由/负载均衡 → 上游站点 → 流式回传 + 用量记账 |
| Token 采集 | 后台 Worker 每 20 秒扫描 18 种工具日志 → 指纹去重 → 规范化 → SQLite 快照 |

---

## 功能模块全景

### 站点资料库（site/library）
- 站点 CRUD、JSON 导入、种子数据初始化（`resources/sites.json`）
- 30+ 字段：API 地址、系统类型（new-api/one-api 等）、注册限制、签到/福利链接、维护者、扩展链接、费率、状态页……
- 多维标记：收藏、隐藏、个人站、待定、跑路（`is_runaway`）、假公益、待举报
- 从远程目录源（ldoh）拉取/同步站点列表与用户信息
- 关键词、分类、收藏状态筛选；系统默认浏览器打开

### Chrome 会话同步（site/sync）
- 自动枚举本机 Chrome Profile，读取 Cookie 会话
- 多站点批量同步账号余额/用量；会话保活与失效恢复
- 支持在指定 Profile 中打开站点、清理同步标签页

### 代理池（proxypool + kernel）
- 订阅源管理：添加/删除/刷新，解析 SS/SSR/V2Ray/Trojan 等节点（Clash 内核 UA 拉取）
- 节点测速（默认 `gstatic generate_204` 探针，可自定义）、并发测速与取消
- GeoIP 归属地分析，国家/地区分组视图，无效节点一键清理
- 代理通道管理：创建通道、绑定节点、为站点账号分配通道
- Mihomo 内核生命周期管理：状态查询、按需下载、更新检查；代理配置持久化与开机自动恢复
- 出站代理规则：默认忽略局域网地址段（localhost/127.0.0.1/私网 CIDR）

### 模型目录（model/catalog）
- 抓取各站点 `/v1/models` 与定价页，标准化入库
- 厂商/能力/价格多维筛选，模型详情与提供商对比
- 以本地日期判断当日是否已同步，跨午夜由前端计时器触发再同步
- 系统字体枚举（font-kit，用于图表渲染）

### 模型网关（model/gateway）
统一入口聚合多个上游站点，兼容三种协议族：

| 端点 | 协议 |
|------|------|
| `POST /v1/chat/completions` | OpenAI Chat Completions |
| `POST /v1/responses` | OpenAI Responses |
| `POST /v1/messages` | Anthropic Messages |
| `GET /v1/models`、`GET /v1/models/{id}` | OpenAI 模型列表 |
| `POST /v1beta/models/{model}:generateContent` 等 | Gemini |

内部机制：
- **路由与负载均衡**：多站点通道聚合，稳定统计 ID，失败转移
- **流式管道**：SSE 增量透传、delta 重建全文（128K 截断）、usage 补全
- **健康检查**：主动探活 + 请求级健康报表（区间前置补偿）
- **统计体系**：请求日志（含响应正文）、渠道日/小时聚合表、平均时延、总览看板
- **OpenCode 代理**：独立的 OpenCode CLI 兼容入口，CLI 会话头模拟、429 动态退避、付费模型拦截校验
- API Key 首次启动自动生成，存于 `app_meta` 配置，可在 Web UI 网关页查看

### Token 统计（token/collector + token/stats）
**本地终端平面**——后台每 20 秒增量扫描以下 18 种 AI 工具的本地日志/数据库：

Claude Code、Codex、Cursor、Cline、Continue、Copilot、Windsurf、Aider、Goose、Kiro、OpenCode、Zed、ZCode、Antigravity、Mimo、Catpawai、CommandCode、DSH

处理管线：文件指纹去重 → 会话/对话/请求三级解析 → 模型规范化（slug 归一）→ 人机消息区分 → SQLite 快照入库。
产出：会话/模型/子代理维度统计、日/小时用量桶、成本估算、请求健康报表、原始日志浏览、本地 Agent 路径探测（支持环境变量覆盖）。

**反代网关平面**——来自网关请求明细与聚合表，见上文模型网关统计体系。

### 公益监控（charity）
- RSS 订阅源管理与定时轮询调度
- 内容聚合、去重、已读标记、未读计数
- 与代理池状态联动（刷新走代理）、同步日志审计

---

## 技术选型

### 前端

| 类别 | 选型 | 版本 |
|------|------|------|
| 框架 | Vue 3（Composition API）+ TypeScript（strict） | 3.5 / 5.6 |
| 构建 | Vite | 6.x |
| 图表 | ECharts | 6.x |
| 表格 | TanStack Vue Table | 9.x |
| 类型检查 | vue-tsc | — |

UI 为完全自研的组件库（表格、下拉、对话框、Toast、右键菜单、Tooltip、日期区间选择等），无第三方 UI 依赖。

### 后端

| 类别 | 选型 | 版本 |
|------|------|------|
| 框架 | Tauri（仅 desktop feature） | 2.8 |
| 语言 | Rust（edition 2021） | — |
| Web 框架 | Axum + tower-http（CORS/fs） | 0.8 |
| 数据库 | rusqlite（bundled SQLite，WAL） | 0.37 |
| 异步运行时 | Tokio（rt-multi-thread） | 1.x |
| HTTP 客户端 | reqwest（native-tls/http2/gzip/brotli/stream） | 0.13 |
| 序列化 | Serde / serde_json / serde_yaml / quick-xml | — |
| GeoIP | maxminddb | 0.30 |
| 压缩 | zstd / flate2 / zip | — |
| 可观测性 | tracing + tracing-subscriber（EnvFilter，`RUST_LOG` 控制） | 0.1 / 0.3 |

### 编译配置要点

- `[lib] crate-type = ["rlib"]`：去掉 staticlib/cdylib，dev 构建少生成约 265MB 的 `.a/.dylib`；
- `profile.dev`：`debug = "line-tables-only"`，第三方依赖 `debug = false`；
- `profile.release`：`lto = "thin"` + `codegen-units = 1` + `strip = "symbols"`。

---

## 代码导览

```
OpenHub/
├── src/                             # 前端（Vue 3）
│   ├── components/
│   │   ├── auth/LoginView.vue       # 登录门禁界面
│   │   ├── common/                  # 自研通用组件（表格/EChart/确认框/引导…）
│   │   ├── layout/AppSidebar.vue    # 侧边栏布局
│   │   ├── pages/                   # 七大页面
│   │   │   ├── SiteLibraryPage.vue  #   站点资料库
│   │   │   ├── ProxyPoolPage.vue    #   代理池
│   │   │   ├── ModelCatalogPage.vue #   模型目录
│   │   │   ├── ModelProxyPage.vue   #   模型网关
│   │   │   ├── TokenStatsPage.vue   #   Token 统计
│   │   │   ├── CharityMonitorPage.vue # 公益监控
│   │   │   └── SettingsPage.vue     #   设置
│   │   └── site/                    # 站点卡片/表单/Chrome 会话对话框…
│   ├── composables/
│   │   ├── core/                    # ★ 运行时核心
│   │   │   ├── ipc.ts               #   clientMode 判定 / runCommand 双通道路由 / RPC 协议
│   │   │   ├── events.ts            #   跨端事件监听（Tauri event / SSE 自动切换+重连）
│   │   │   ├── capabilities.ts      #   /api/caps 能力协商合并
│   │   │   └── useTheme.ts …        #   主题/偏好/Toast
│   │   ├── site/ proxy/ model/ token/ charity/ ui/   # 各域业务逻辑
│   │   └── *.ts                     # （根级为旧版平铺 re-export，逐步迁移中）
│   ├── types.ts / constants.ts / utils.ts
│   └── main.ts                      # 入口：登录门禁 → 加载 caps → 挂载 App
├── src-tauri/
│   ├── src/
│   │   ├── main.rs                  # desktop 二进制入口（调 lib::run）
│   │   ├── bin/server.rs            # server 二进制入口（参数解析/装配/启动）
│   │   ├── lib.rs                   # ★ 模块声明 + Tauri 装配 + invoke_handler 命令表
│   │   ├── core/
│   │   │   ├── context.rs           # AppContext / EventBus / LoginManager / Capabilities
│   │   │   ├── db.rs                # 全部建表语句与迁移（23+ 张表）
│   │   │   ├── models.rs            # SiteRecord 等核心数据结构与常量
│   │   │   ├── web_server.rs        # ★ 统一 HTTP 层：RPC 分发/SSE/caps/静态资源/登录会话
│   │   │   ├── single_instance.rs   # 单实例互斥
│   │   │   └── app_menu.rs file_export.rs
│   │   ├── site/library/            # 站点 CRUD/导入/远程同步/平台识别
│   │   ├── site/sync/               # Chrome 会话检测/账号同步/用量
│   │   ├── proxypool/               # parser/tester/rotator/runtime/geoip
│   │   ├── kernel/                  # mihomo.rs / geoip.rs —— 组件按需下载引导
│   │   ├── model/catalog/           # 模型目录聚合
│   │   ├── model/gateway/           # router/pipeline/dispatcher/balancer/stream/logger/stats
│   │   │   └── handlers/            #   chat / responses / anthropic / gemini 协议适配
│   │   ├── token/collector/         # 18 种工具日志采集器 + normalizer/aggregator
│   │   ├── token/stats/             # 统计入库/查询/worker/health
│   │   ├── charity/                 # RSS feed/fetcher/scheduler
│   │   └── tests/                   # 集成测试
│   ├── Cargo.toml                   # ★ 双形态 feature 定义与编译配置
│   └── tauri.conf.json              # 窗口/CSP/资源打包/签名配置
├── scripts/
│   ├── check-version.mjs            # 三处版本号一致性校验
│   ├── package-server.mjs           # server 发布包组装（二进制+dist+README+sha256）
│   └── clean-target.sh              # 构建缓存清理（--deep 深度模式）
├── .github/workflows/
│   ├── build.yml                    # 四平台构建矩阵
│   └── release.yml                  # tag 触发 Release
├── public/                          # 静态资源（logo/sites.json 种子）
└── vite.config.ts                   # 固定端口 1420，HMR，排除 src-tauri 监听
```

---

## 开发指南

### 前置要求

| 工具 | 版本 | 用途 |
|------|------|------|
| Node.js | 18+（CI 使用 22） | 前端构建 |
| Rust | stable（edition 2021） | 后端编译 |
| Tauri 2 系统依赖 | — | Linux 需 webkit2gtk-4.1 / gtk-3 等，macOS 需 Xcode CLT |

### 安装依赖

```bash
npm ci        # CI 与发布必须用 lockfile 安装；日常也可 npm install
```

### 一体式桌面客户端开发

```bash
npm run integrated:dev     # 即 tauri dev；desktop 是兼容别名
```

启动内容：Vite dev server（:1420）+ 默认二进制 `open-hub-desktop`，包含本地 SQLite、Tauri IPC、本地 Token 日志采集、模型网关和内嵌 HTTP 服务。首次启动 Mihomo/GeoIP 由组件初始化引导按需下载。

#### dev 与正式版自动隔离

`tauri dev` 是 debug 构建，与 `tauri build` 出的正式版（release 构建）自动隔离，可同时运行互不干扰：

| 资源 | 正式版 | dev |
|---|---|---|
| Web UI / 模型网关端口 | `17896` | `17996` |
| 数据目录 | `…/com.dfeer.openhub.desktop` | `…/com.dfeer.openhub.desktop-dev` |
| 数据库 / 配置 / 缓存 / 代理运行时 | 共用正式数据 | 全新独立 |
| 单实例锁 | 只管理正式版进程 | 只管理 dev 进程 |

因此开发调试不会污染正式版数据，也不会把正在使用的正式版进程挤掉线。判据是构建 profile，需要临时反转时可用环境变量覆盖：`OPENHUB_PROFILE=dev|release`；dev 形态的窗口标题会显示为「OpenHub (dev)」以便区分。

### 仅前端开发（静态预览模式）

```bash
npm run dev        # Vite only；无 Tauri 时自动进入 web 模式并使用浏览器兜底数据
```

### 客户端运行面控制

```bash
# 强制以某种形态构建前端（默认自动检测 __TAURI_INTERNALS__）
VITE_OPENHUB_CLIENT_MODE=thin npm run build
```

### 常用命令

```bash
npm run build            # vue-tsc 类型检查 + vite 产物构建
npm run check:version    # 校验 package.json / Cargo.toml / tauri.conf.json 版本一致
npm run clean:target     # 清理旧二进制与增量缓存
npm run clean:deep       # 深度清理（含 .o 文件与 release 目录）
cargo test               # 运行 Rust 单元/集成测试（见下文）
cargo clippy             # lint
```

### 日志

统一使用 tracing，格式 `时间 级别 目标: 消息`，可用环境变量过滤：

```bash
RUST_LOG=debug npm run integrated:dev
RUST_LOG=open_hub_desktop_lib::model=trace ./openhub-server
```

### 扩展新模块的标准路径

1. `src-tauri/src/<module>/` 创建 Rust 模块（commands/db/types 分层）；
2. 在 `core/db.rs` 注册建表与迁移；
3. 命令同时挂到两处：`lib.rs` 的 `invoke_handler![]`（IPC）与 `core/web_server.rs` 的 RPC 命令表（HTTP）；
4. `src/composables/<module>/` 创建组合式函数，业务调用一律走 `runCommand()`；
5. `src/components/pages/` 创建页面并接入侧边栏。

> 只做第 3 步的一半会导致形态间行为不一致：桌面能用而 Web 报"未知命令"，或反之。

---

## 构建与打包

### 桌面安装包

```bash
npm run integrated:build   # 即 tauri build；desktop:build 是兼容别名
```

产物位于 `src-tauri/target/release/bundle/`：
- macOS：`.app` / `.dmg`（ad-hoc 签名，最低 10.13）
- Windows：`.msi` / NSIS `.exe`
- Linux：`.AppImage` / `.deb` / `.rpm`

安装包只内置本地化资源与 `dist/` 前端产物；Mihomo 与 GeoIP 不进包。

### 独立服务端

```bash
# 1. 构建前端
npm run build

# 2. 构建无窗口依赖的单文件二进制
cargo build --manifest-path src-tauri/Cargo.toml \
  --release --no-default-features --features server --bin openhub-server

# 3. 组装发布包（二进制 + dist/ + README.txt + sha256）
SERVER_PLATFORM=darwin SERVER_ARCH=arm64 npm run package:server
# 输出到 dist-server/openhub-server-darwin-arm64/
```

发布包目录结构（`dist/` 必须与二进制同级，或用 `--dist-dir` 指定）：

```
openhub-server-darwin-arm64/
├── openhub-server
├── dist/                 # 前端静态资源
├── README.txt
├── openhub-server.sha256
└── README.txt.sha256
```

---

## 部署指南

### 形态一：桌面客户端（个人使用推荐）

下载对应平台的安装包（Actions artifact 或 Release），常规安装即可。数据保存在：

```text
macOS:   ~/Library/Application Support/com.dfeer.openhub.desktop/
Windows: %APPDATA%\com.dfeer.openhub.desktop\
Linux:   ~/.local/share/com.dfeer.openhub.desktop\
```

应用自带内嵌 HTTP 服务（默认 `127.0.0.1:17896`），可在浏览器打开同源 Web 界面。

关闭主窗口不会退出应用：窗口隐藏，macOS 底部 Dock 图标同步收起，仅保留菜单栏托盘图标，后台继续提供 Web UI 与模型网关服务。点菜单栏托盘图标（左键单击或右键菜单「显示主窗口」）可再次打开窗口；彻底退出请用托盘菜单「退出 OpenHub」或 `Cmd+Q`。

### 形态二：独立服务端（NAS / VPS / 家庭服务器）

#### 快速启动

```bash
tar xzf openhub-server-linux-x64.tar.gz
cd openhub-server-linux-x64

# 本机回环监听（默认端口 17896，被占用自动顺延，最多尝试 24 个端口）
./openhub-server

# 对外提供服务（所有请求仍要求登录会话）
./openhub-server --host-all --listen 17896

# 显式指定监听地址（IPv6 用方括号）
./openhub-server --listen 192.0.2.10:17896
./openhub-server --listen "[::1]:17896"

# 自定义数据目录与登录凭据
OPENHUB_LOGIN_USER=me OPENHUB_LOGIN_PASSWORD='s3cret' \
  ./openhub-server --data-dir ~/openhub-data
```

全部参数：

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `--listen <port\|ip:port\|[ipv6]:port>` | `17896`（回环） | 监听地址 |
| `--host-all` | — | 纯端口形式改绑 `0.0.0.0` |
| `--data-dir <dir>` | 平台应用数据目录 | SQLite、auth-sessions.json、proxy-runtime 存放处 |
| `--dist-dir <dir>` | 二进制旁 `dist/` 或 `./dist` | 前端静态资源 |
| `--user` / `--password` | `admin` / `Admin@2026` | 登录凭据（环境变量优先） |

首次启动后访问 `http://<host>:<port>/`，用上述凭据登录；Session 保存在浏览器 localStorage，默认 7 天有效。

#### systemd 生产部署示例（Linux）

```ini
# /etc/systemd/system/openhub.service
[Unit]
Description=OpenHub Server
After=network-online.target

[Service]
User=openhub
WorkingDirectory=/opt/openhub                 # 二进制与 dist/ 所在目录
ExecStart=/opt/openhub/openhub-server --listen 17896
Environment=OPENHUB_LOGIN_USER=admin
Environment=OPENHUB_LOGIN_PASSWORD=change-me
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl enable --now openhub
```

#### 安全建议

- **务必修改默认密码**（`Admin@2026`），或通过环境变量注入强凭据；
- 对公网暴露时建议前置 Nginx/Caddy 做 TLS 终结，后端保持回环或内网监听；
- 定期备份 `--data-dir`（核心是 `sites.sqlite3`，可选 `auth-sessions.json`）。

#### 升级

替换二进制与 `dist/` 后重启进程即可；数据库 schema 由程序在启动时自动迁移，无需手工干预。

---

## Web API 参考

三种流量共享一个监听端口，按路径与凭据区分：

| 流量类型 | 路径 | 凭据 |
|----------|------|------|
| Web UI | `GET /*`（SPA fallback） | 登录 Session（Cookie 场景外均可） |
| 业务 RPC | `POST /api/rpc` | `X-OpenHub-Token` 或 `Authorization: Bearer`（登录 Session） |
| 事件流 | `GET /api/events`（SSE） | 同上 |
| 能力协商 | `GET /api/caps` | 同上 |
| 模型 API | `/v1/*`、`/v1beta/*` | 网关 API Key（仅请求头：`Authorization: Bearer` / `x-api-key`，**不接受 URL 参数**） |

### 登录会话

```bash
# 登录换取 Session Token
curl -X POST http://127.0.0.1:17896/api/rpc \
  -H 'Content-Type: application/json' \
  -d '{"command":"login","args":{"username":"admin","password":"Admin@2026"}}'
# → {"data":"<token>"}

# 携带 Token 调用业务命令（与桌面 IPC 同名）
curl -X POST http://127.0.0.1:17896/api/rpc \
  -H 'Content-Type: application/json' \
  -H 'X-OpenHub-Token: <token>' \
  -d '{"command":"list_library","args":{}}'
```

- Session 有效期 7 天，服务端持久化为 SHA256 哈希（`auth-sessions.json`），重启不失效；
- 401 时前端统一抛出 `AUTH_REQUIRED` 并跳转登录；
- 本地数据平面命令（`get_token_stats`、`sync_token_data` 等 6 个）在 HTTP 层被硬拒绝，Web 服务永远不会代替访问者读取其本机 AI 日志或 Chrome Profile。

### RPC 协议细节

- 请求体上限 16 MB；
- 参数提取支持 camelCase / snake_case 双别名；
- 响应遵循 `{"data": ...}` / `{"error": "...", "code": "..."}` 结构，`Ok/Err` 包装自动解包。

### SSE 事件流

帧格式 `event: <name>\n data: {"event":"...","payload":...}`，断线 1.5 秒自动重连；带 `Accept: text/event-stream` 头用 fetch 流消费（便于携带鉴权头）。

---

## 数据库设计

单文件 SQLite（`sites.sqlite3`，WAL 模式，运行期伴随 `-wal/-shm` 文件属正常现象）。主要表分组：

| 分组 | 表 |
|------|-----|
| 站点资料库 | `directory_sites`、`site_tags`、`site_maintainers`、`site_extensions`、`site_accounts` |
| 模型目录 | `model_catalog_sources`、`model_catalog_providers`、`model_catalog_models`、`site_model_cache` |
| 模型网关 | `model_proxy_logs`、`channel_daily_stats`、`channel_hourly_stats` |
| 代理池 | `proxy_subscriptions`、`proxy_nodes`、`proxy_pool_nodes`、`proxy_subscription_nodes`、`proxy_channels`、`account_proxy_channels` |
| Token 统计 | `token_cache_snapshots` |
| 公益监控 | `charity_feed_items`、`charity_feed_sources`、`charity_sync_logs` |
| 应用配置 | `app_meta`（网络代理、活动节点、网关 API Key 等键值配置） |

所有 DDL 位于 `src-tauri/src/core/db.rs`，启动时执行幂等建表与迁移。

---

## 测试与质量

测试集中在 Rust 侧（约 195 个 `#[test]`），覆盖：

| 模块 | 重点 |
|------|------|
| `core/tests` | 数据库迁移、站点 CRUD、导入导出 |
| `site/sync` | Chrome 会话解析、存储、同步流程 |
| `proxypool/tests` | 订阅解析、测速逻辑、GeoIP |
| `model/gateway/tests` | 协议适配、流式重建、统计聚合 |
| `token/collector/tests` | 18 种来源的解析器回归 |
| `charity/tests` | Feed 解析与调度 |

```bash
cd src-tauri && cargo test          # 全量测试
cd src-tauri && cargo test gateway  # 按模块过滤
```

前端质量门禁为 `vue-tsc` 严格类型检查（随 `npm run build` 执行）；提交遵循 Conventional Commits（历史提交均为 `feat:/fix:/docs:/ci:` 风格）。

---

## CI/CD 发布流程

### 工作流

| 工作流 | 触发 | 行为 |
|--------|------|------|
| `build.yml` | push 到 `main`、PR、手动 | 四平台矩阵构建，上传 artifact（保留 14 天） |
| `release.yml` | 推送 `v*` tag 或手动指定 tag | 复用构建矩阵，汇总产物创建 GitHub Release（自动生成 notes + SHA256SUMS） |

### 构建矩阵

| 名称 | Runner | 产物 |
|------|--------|------|
| macOS Intel | `macos-13` | 桌面包 + `openhub-server-darwin-x64.tar.gz` |
| macOS Apple Silicon | `macos-14` | 桌面包 + `openhub-server-darwin-arm64.tar.gz` |
| Windows x64 | `windows-latest` | 桌面包 + `openhub-server-win32-x64.zip` |
| Linux x64 | `ubuntu-22.04` | 桌面包 + `openhub-server-linux-x64.tar.gz` |

每个平台均产出：Tauri 桌面安装包、`openhub-server` 二进制 + 同目录 `dist/` 压缩包、`SHA256SUMS` 校验文件。

### 发版步骤

```bash
# 1. 统一三处版本号（不一致会在 CI 直接失败）
vim package.json src-tauri/Cargo.toml src-tauri/tauri.conf.json
npm run check:version

# 2. 打 tag 推送，release.yml 自动接管
git tag v0.3.1
git push origin v0.3.1
```

### 签名现状

当前发布包采用开发/未签名策略：macOS ad-hoc、Windows 无 Authenticode、Linux 包未签名；未启用 Tauri updater。公开分发前应在 GitHub Secrets 配置 Developer ID/公证凭据与 Windows 证书并在工作流启用对应步骤。

---

## 安全模型

| 层面 | 机制 |
|------|------|
| 传输边界 | 本地终端 Token 日志只在本机采集与展示，任何形态下都不经网络上传 |
| 访问门禁 | 所有 Web RPC/SSE/caps 要求有效登录 Session；对外监听也不例外 |
| 凭据存储 | 密码支持环境变量注入；Session 仅存哈希；模型 API Key 只走请求头 |
| 权限最小化 | 本机文件类能力（Chrome Profile/AI 日志）不进入 RPC 命令表，`/api/caps` 对 Web 恒报关闭 |
| WebView CSP | `script-src 'self'`；asset 协议限定 `$APPDATA/$APPCONFIG/$APPLOCALDATA/$RESOURCE` 范围 |
| 单实例 | 数据目录锁防止双实例并发写库 |

---

## 故障排除

<details>
<summary><b>数据库锁定错误</b></summary>

```
Error: database is locked
```
关闭其他 OpenHub 实例（桌面端有单实例保护，注意是否有残留 server 进程占用同一 data-dir），或重启应用。
</details>

<details>
<summary><b>Chrome 会话检测失败</b></summary>

确保 Chrome 已退出（锁定的 Cookie 数据库无法读取），并检查用户数据目录权限。
</details>

<details>
<summary><b>代理节点测速超时</b></summary>

检查订阅源可达性与测速探针 URL（设置中可自定义）；必要时先完成 Mihomo/GeoIP 组件初始化（代理池页面可重试下载）。
</details>

<details>
<summary><b>模型目录/订阅同步失败</b></summary>

查看日志定位：`RUST_LOG=debug` 启动，或查看应用日志输出。部分订阅服务按 UA 区分客户端，OpenHub 已使用 Clash 内核 UA 拉取。
</details>

<details>
<summary><b>端口被占用</b></summary>

服务端从首选端口向后顺延最多 24 次，实际端口打印在启动日志；刚杀掉旧实例时会短暂轮询等待端口释放。
</details>

<details>
<summary><b>重置应用数据</b></summary>

```bash
# macOS（桌面端）
rm -rf ~/Library/Application\ Support/com.dfeer.openhub.desktop/

# 独立 server：删除对应 --data-dir 即可
```
</details>

---

## 许可证

私有项目，仅供个人使用与学习研究。使用者需遵守当地法律法规；开发者不对使用本软件造成的任何后果负责。

## 致谢

- [Tauri](https://tauri.app/) — 跨平台桌面应用框架
- [Vue](https://vuejs.org/) — 渐进式 JavaScript 框架
- [Axum](https://github.com/tokio-rs/axum) — Rust Web 框架
- [SQLite](https://sqlite.org/) — 嵌入式数据库
- [Mihomo](https://github.com/MetaCubeX/mihomo) — 代理内核
