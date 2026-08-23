# OpenHub

使用 Tauri 2 + Vue 3 + SQLite 构建的本地站点资料库，支持一体式桌面客户端与独立 Web 服务。瘦客户端的双通道运行时已经在前端协议层就绪：远程业务走 HTTP RPC/SSE，本地 Token 统计走客户端 IPC；独立瘦客户端壳和物理 LocalTokenStore 拆分仍属于后续发布阶段。

## 产品与数据边界

OpenHub 有两套互不默认合并的数据平面：

- **本地终端 Token**：读取当前客户端所在设备上的 Claude、Codex、Cursor、Cline、Continue 等 AI 工具日志，仅在客户端本地展示，永不上传到 Web 服务；直连第三方的 AI 终端只能通过这套本地日志统计。
- **反代网关 Token**：只统计经过 OpenHub 模型网关的请求，由服务端保存并通过桌面端、瘦客户端和浏览器展示。它不覆盖终端绕过 OpenHub 的直连流量，也不与本地终端统计自动相加。

独立 Web 浏览器只能访问服务端数据平面。Web 服务不会读取访问者本机的 AI 日志、Chrome Profile、Cookie、桌面文件或系统字体。

## 双形态架构

```
┌──────────────────────────────────────────────────────────┐
│ 一体式桌面客户端                                         │
│ 本地业务内核 + 本地终端 Token + Chrome + 网关 + Tauri IPC │
└──────────────────────────────────────────────────────────┘

┌──────────────────────┐       HTTP RPC + SSE       ┌──────────────────────┐
│ 瘦客户端（双通道）   │ ─────────────────────────▶ │ openhub-server       │
│ 本地 Token → IPC     │                            │ Web UI + 反代统计    │
│ 远程业务 → HTTP      │                            │ 站点/代理/模型网关    │
└──────────────────────┘                            └──────────────────────┘

独立浏览器直接访问 openhub-server 时，只使用右侧服务端能力。
```

- **桌面形态**（默认）：`npm run desktop:build`，体验与常规桌面应用一致；
- **服务形态**：`cargo build --no-default-features --features server`，
  产物 `target/release/openhub-server` 为无窗口依赖的单二进制，
  可部署到本机、NAS 或 VPS：

```bash
# 服务端 API/UI：默认回环监听 17896；打开 Web 页面后登录
openhub-server --listen 17896

# 对外监听仍要求登录会话
openhub-server --host-all --listen 17896

# 也可以显式指定监听地址（IPv6 使用方括号）
openhub-server --listen 192.0.2.10:17896
openhub-server --listen "[::1]:17896"

openhub-server --data-dir ~/openhub-data --help      # 查看全部参数
```

浏览器访问 `http://<host>:<port>/` 使用服务端界面，登录成功后 Session 会保存在浏览器本地，默认有效期 7 天。所有 Web RPC 和事件流请求都必须携带有效登录 Session。

服务端模型接口与 Web UI/API 共享 `openhub-server` 的监听端口。Web UI 和 `/api/*` 使用登录 Session；`/v1/*` 与 `/v1beta/*` 使用独立模型 API Key。API Key 只能通过请求头发送，不支持 URL 查询参数。

Chrome 会话同步、本地终端 Token 日志等依赖客户端本机文件的能力不会通过 `/api/rpc` 暴露；`/api/caps` 对 Web 固定返回这些能力为关闭。

## 核心功能

### 站点管理
- 新增、编辑和删除站点记录
- 保存站点名称、URL、分类、标签和备注
- 收藏常用站点
- 按关键词、分类和收藏状态筛选
- 调用系统默认浏览器打开站点

### 多账号同步
- Chrome 浏览器会话自动检测
- 多站点批量同步
- 自动会话保活与失效恢复
- 定时自动同步（可配置间隔）

### 代理池管理
- 订阅源管理（添加/删除/刷新）
- 代理节点测速与筛选
- 国家/地区分组视图
- GeoIP 数据库自动下载
- 代理通道管理

### 模型目录
- AI 模型信息聚合
- 多维度筛选（厂商、能力、价格）
- 模型详情与提供商对比
- 每日自动同步

### 模型网关
- 本地反向代理服务
- 多站点通道聚合
- 健康检查与日志
- OpenCode 代理支持

### 公益监控
- RSS 订阅源管理
- 内容聚合与已读标记
- 代理池状态联动

### Token 统计

页面提供两个明确的数据来源标签：

- **本地终端**：当前客户端本机 AI 工具日志与本地 SQLite 快照；一体式客户端和瘦客户端可用，独立 Web 不可用。
- **反代网关**：OpenHub 模型网关请求明细与日/小时聚合；桌面端、瘦客户端和独立 Web 均可用。

两套数据源共用展示组件，但不共用采集任务，不默认相加，也不把同一终端经网关转发时产生的两份记录强行去重。

### 界面特性
- 明暗主题切换
- 侧边栏折叠
- 右键菜单（中文）
- 工具提示
- Toast 通知
- 快捷键支持（⌘K 搜索）

## 技术栈

### 前端
- **框架**: Vue 3.5 + TypeScript
- **构建工具**: Vite 6
- **UI 组件**: 自定义组件库
- **图表**: ECharts 6
- **表格**: TanStack Vue Table 9

### 后端
- **框架**: Tauri 2.8
- **语言**: Rust (Edition 2021)
- **数据库**: SQLite (rusqlite 0.37)
- **HTTP 客户端**: reqwest 0.13
- **异步运行时**: Tokio 1
- **Web 框架**: Axum 0.8
- **序列化**: Serde + serde_json
- **压缩**: zstd 0.13

### 系统集成
- **系统集成**: Mihomo 代理内核和 GeoIP 数据库采用首次使用时按需下载，不随安装包内置；
- **文件对话框**: rfd 0.17
- **字体检测**: font-kit 0.14

## 项目结构

```
OpenHub/
├── src/                          # 前端源码
│   ├── components/
│   │   ├── common/               # 通用组件
│   │   ├── layout/               # 布局组件
│   │   ├── pages/                # 页面组件
│   │   └── site/                 # 站点相关组件
│   ├── composables/              # Vue 组合式函数
│   │   ├── charity/              # 公益监控
│   │   ├── core/                 # 核心功能
│   │   ├── model/                # 模型相关
│   │   ├── proxy/                # 代理池
│   │   ├── site/                 # 站点管理
│   │   ├── token/                # Token 统计
│   │   └── ui/                   # UI 状态
│   ├── styles.css                # 全局样式
│   ├── types.ts                  # TypeScript 类型定义
│   ├── utils.ts                  # 工具函数
│   └── main.ts                   # 入口文件
├── src-tauri/                    # 后端源码
│   ├── src/
│   │   ├── charity/              # 公益监控模块
│   │   ├── core/                 # 核心模块
│   │   ├── kernel/               # 内核管理
│   │   ├── model/                # 模型目录/网关
│   │   ├── proxypool/            # 代理池
│   │   ├── site/                 # 站点管理
│   │   └── token/                # Token 统计
│   ├── resources/                # 资源文件
│   ├── icons/                    # 应用图标
│   └── tauri.conf.json           # Tauri 配置
├── scripts/                      # 构建脚本
├── public/                       # 静态资源
└── package.json                  # 前端依赖
```

## 数据库

应用第一次启动时会在 Tauri 的应用数据目录创建：

```text
sites.sqlite3
```

macOS 通常位于：

```text
~/Library/Application Support/com.dfeer.openhub.desktop/sites.sqlite3
```

数据库启用了 WAL 模式，运行期间还可能出现 `sites.sqlite3-wal` 和 `sites.sqlite3-shm` 文件，这是 SQLite 的正常行为。

### 主要表结构

`sites` 表字段：
- `id` - 唯一标识
- `name` - 站点名称
- `url` - 站点地址
- `category` - 分类
- `description` - 描述
- `tags` - 标签（JSON 字符串）
- `favorite` - 是否收藏
- `created_at` - 创建时间
- `updated_at` - 更新时间

## 开发环境

### 前置要求

- Node.js 18+
- Rust 1.70+
- Tauri CLI 2.x

### 安装依赖

```bash
npm ci
```

开发环境也可以使用 `npm install`，但 CI 和发布构建必须使用已提交的 `package-lock.json` 执行 `npm ci`。

### 一体式客户端开发

调试完整的一体式 Tauri 客户端请使用：

```bash
npm run integrated:dev
```

该命令启动默认的 `open-hub-desktop` 二进制，包含本地 SQLite、Tauri IPC、本地 Token 日志采集、模型网关和内嵌 HTTP 服务。首次启动时 Mihomo 和 GeoIP 会通过组件初始化引导按需下载。

`npm run desktop` 仍然保留为兼容别名，效果相同。

### 构建应用

```bash
npm run integrated:build
```

`npm run desktop:build` 仍然保留为兼容别名。

构建产物位于：

```text
src-tauri/target/release/bundle/
```

### 清理构建缓存

```bash
npm run clean:target      # 清理旧二进制和增量缓存
npm run clean:deep        # 深度清理（包括 .o 文件）
```

## GitHub Actions 自动打包

仓库包含两条发布工作流：

- `.github/workflows/build.yml`：推送到 `main`、创建 Pull Request 或手动运行时，在原生 runner 上构建 macOS Intel、macOS Apple Silicon、Windows x64 和 Linux x64 产物，并上传 GitHub Actions artifact。
- `.github/workflows/release.yml`：推送 `v0.3.0` 形式的版本标签时，复用同一套矩阵构建并创建 GitHub Release；也可以通过 Actions 页面手动指定已有 tag。

每个平台会生成：

- Tauri 桌面安装包（macOS `.dmg`、Windows `.msi`/NSIS、Linux `.AppImage`/`.deb`/`.rpm`，以 runner 实际生成结果为准）；
- `openhub-server` 与同目录 `dist/` 前端资源的压缩包；
- `SHA256SUMS` 校验文件。

发布前请先把 `package.json`、`src-tauri/Cargo.toml` 和 `src-tauri/tauri.conf.json` 的版本统一，再创建 tag：

```bash
npm run check:version
git tag v0.3.0
git push origin v0.3.0
```

工作流不会把 Mihomo 或 GeoIP 二进制资源写入安装包。首次打开时由组件初始化引导按当前平台和架构按需下载；组件版本和下载地址由 Rust 运行时管理，更新失败不会影响主程序启动。

当前发布包仍使用开发/未签名策略：macOS 为 ad-hoc 签名，Windows 未配置 Authenticode，Linux 包未签名。公开分发前应在 GitHub Secrets 中配置 Apple Developer ID/公证凭据和 Windows 代码签名证书，并在工作流中启用对应签名步骤。当前没有启用 Tauri updater，因此不需要 updater 私钥。

## 独立 server 包

`openhub-server` 包不是把 Web 页面嵌入二进制，而是和 sibling `dist/` 目录一起发布。解压后应在包含 `dist/` 的目录运行二进制，或通过 `--dist-dir` 指定前端目录。

独立 server 默认启动登录门禁；对外监听与回环监听都必须使用有效登录会话，Session 默认有效期 7 天。


| 快捷键 | 功能 |
|--------|------|
| ⌘K / Ctrl+K | 聚焦搜索框 |
| Escape | 关闭弹窗/返回 |

## 平台支持

- macOS (主要开发平台)
- Windows
- Linux

### macOS 签名

当前使用 ad-hoc 签名用于本地测试。公开分发时应配置 Developer ID 并完成 notarization。

## 配置文件

### Tauri 配置

`src-tauri/tauri.conf.json` 包含应用配置：
- 窗口尺寸与行为
- 安全策略 (CSP)
- 资源打包
- 构建命令

### TypeScript 配置

`tsconfig.json` 配置了严格的类型检查和 Vue 支持。

### Vite 配置

`vite.config.ts` 针对 Tauri 开发进行了优化：
- 固定端口 1420
- HMR 支持
- 排除 src-tauri 目录监听

## 核心模块说明

### 站点同步 (sync)
- Chrome 浏览器会话检测
- Cookie 与 API Key 提取
- 账号状态监控
- 自动保活机制

### 代理池 (proxypool)
- 订阅源解析（SS/SSR/V2Ray/Trojan）
- 节点测速与筛选
- GeoIP 分析
- 通道管理

### 模型目录 (catalog)
- 多源数据聚合
- 模型信息标准化
- 价格对比分析
- 每日增量同步

### 模型网关 (gateway)
- 本地反向代理
- 多站点通道聚合
- 请求路由与负载均衡
- 健康检查

### Token 统计 (token)
- 多工具日志解析
- 会话/对话/请求分析
- 成本估算
- 效率指标计算

### 公益监控 (charity)
- RSS 订阅源管理
- 内容聚合与去重
- 已读状态跟踪
- 代理池状态联动

## 资源文件

安装包只内置本地化资源。Mihomo 代理内核和 GeoIP 数据库在首次打开时由组件初始化引导按当前平台和架构按需下载，安装后保存在应用数据目录的 `bin/` 与 `Country.mmdb` 路径中，不进入源码仓库和安装包。

无网络时主界面仍可启动，但代理池、测速和节点地域解析会在组件初始化完成前不可用；可在代理池管理中重试下载。

## 开发脚本

### clean-target.sh

清理构建缓存的脚本：
- 默认模式：清除旧二进制、增量缓存、.d 文件
- `--deep` 模式：额外清除 .o 文件和 release 目录

## 环境变量

应用支持以下环境变量配置：

```bash
# Tauri 开发主机（用于 HMR）
TAURI_DEV_HOST=your-ip-address

# Rust 编译优化
RUSTFLAGS="-C target-cpu=native"
```

## 故障排除

### 常见问题

#### 1. 数据库锁定错误
```
Error: database is locked
```
**解决方案**: 关闭其他 OpenHub 实例，或重启应用。

#### 2. Chrome 会话检测失败
```
Error: Chrome profile not found
```
**解决方案**: 
- 确保 Chrome 浏览器已关闭
- 检查 Chrome 用户数据目录权限

#### 3. 代理节点测速超时
```
Error: connection timeout
```
**解决方案**:
- 检查网络连接
- 尝试更换代理节点
- 增加超时时间设置

#### 4. 模型目录同步失败
```
Error: fetch failed
```
**解决方案**:
- 检查网络连接
- 验证 API 端点可达性
- 查看应用日志获取详细错误

### 日志查看

应用日志位于：
- **macOS**: `~/Library/Logs/OpenHub/`
- **Windows**: `%APPDATA%\OpenHub\logs\`
- **Linux**: `~/.local/share/OpenHub/logs/`

### 重置应用数据

如需重置应用，删除以下目录：
```bash
# macOS
rm -rf ~/Library/Application\ Support/com.dfeer.openhub.desktop/

# Windows
rd /s /q %APPDATA%\com.dfeer.openhub.desktop

# Linux
rm -rf ~/.local/share/com.dfeer.openhub.desktop/
```

## 性能优化

### 数据库优化
- 启用 WAL 模式提高并发性能
- 定期清理过期数据
- 优化查询索引

### 内存管理
- 大数据集分页加载
- 图片懒加载
- 组件按需渲染

### 网络优化
- 请求缓存
- 连接池复用
- 压缩传输

## 安全注意事项

### 数据安全
- 所有数据本地存储，不上传云端
- SQLite 数据库文件权限限制
- 敏感信息加密存储

### 网络安全
- CSP 策略限制资源加载
- HTTPS 强制（远程资源）
- 代理连接加密

### 应用安全
- 代码签名验证
- 权限最小化原则
- 定期安全更新

## 贡献指南

### 开发流程
1. Fork 项目仓库
2. 创建功能分支 (`git checkout -b feature/AmazingFeature`)
3. 提交更改 (`git commit -m 'Add some AmazingFeature'`)
4. 推送到分支 (`git push origin feature/AmazingFeature`)
5. 创建 Pull Request

### 代码规范
- **前端**: ESLint + Prettier
- **后端**: rustfmt + clippy
- **提交**: Conventional Commits

### 测试要求
- 新功能必须包含测试
- 测试覆盖率 > 80%
- 所有测试必须通过

## 更新日志

### [0.3.0] - 2024-XX-XX
#### 新增
- 模型网关反向代理服务
- OpenCode 代理支持

#### 改进
- 代理池测速性能优化
- 模型目录同步稳定性
- UI 响应速度提升

#### 修复
- Chrome 会话检测偶发失败
- 代理节点名称显示异常
- 某些情况下搜索无响应

### [0.2.0] - 2024-XX-XX
#### 新增
- 多账号同步功能
- 代理池管理
- 公益监控模块

#### 改进
- 站点管理界面优化
- 数据库查询性能
- 主题切换动画

### [0.1.0] - 2024-XX-XX
#### 新增
- 基础站点管理
- 分类与标签系统
- 收藏功能
- 明暗主题

## 架构设计

### 前端架构
```
┌─────────────────────────────────┐
│         Vue 3 Application       │
├─────────────────────────────────┤
│  Components (UI Layer)          │
├─────────────────────────────────┤
│  Composables (Business Logic)   │
├─────────────────────────────────┤
│  Tauri API (IPC Layer)          │
└─────────────────────────────────┘
```

### 后端架构
```
┌─────────────────────────────────┐
│         Tauri Runtime           │
├─────────────────────────────────┤
│  Core Modules (Rust)            │
│  ├── Site Management            │
│  ├── Proxy Pool                 │
│  ├── Model Catalog              │
│  ├── Token Statistics           │
│  └── Charity Monitor            │
├─────────────────────────────────┤
│  SQLite Database                │
└─────────────────────────────────┘
```

### 数据流
```
User Input → Vue Component → Composable → Tauri Command → Rust Handler → SQLite
     ↑                                                                    │
     └────────────────────────── State Update ←────────────────────────────┘
```

## 扩展开发

### 添加新模块
1. 在 `src-tauri/src/` 创建模块目录
2. 在 `src/composables/` 创建对应的组合式函数
3. 在 `src/components/pages/` 创建页面组件
4. 在 `src-tauri/src/lib.rs` 注册模块
5. 更新 `tauri.conf.json` 配置

### 自定义主题
编辑 `src/styles.css` 修改 CSS 变量：
```css
:root {
  --primary-color: #ffb103;
  --background-color: #ffffff;
  --text-color: #000000;
  /* 更多变量... */
}
```

### 插件系统
应用支持插件扩展，参考 `src-tauri/src/` 目录结构创建插件。

## 致谢

### 开源项目
- [Tauri](https://tauri.app/) - 跨平台桌面应用框架
- [Vue](https://vuejs.org/) - 渐进式 JavaScript 框架
- [SQLite](https://sqlite.org/) - 轻量级数据库
- [Mihomo](https://github.com/MetaCubeX/mihomo) - 代理内核

### 贡献者
感谢所有为项目做出贡献的开发者。

## 联系方式

- **问题反馈**: GitHub Issues
- **功能建议**: GitHub Discussions
- **邮件联系**: [待添加]

## 许可证

私有项目，仅供个人使用。

**免责声明**: 本软件仅供学习和研究使用，用户需遵守当地法律法规。开发者不对使用本软件造成的任何后果负责。
