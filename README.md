# OpenHub

使用 Tauri 2 + Vue 3 + SQLite 构建的本地站点资料库桌面应用。

## 产品定位

这不是线上网站套壳，也不是远程服务状态导航。应用启动后直接进入本地资料库，站点记录只保存在当前设备。

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
- 多工具本地日志解析
- 会话/对话/请求三级统计
- 按模型/来源维度分析
- 成本估算与效率指标

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
- **内核**: Mihomo (代理核心)
- **地理数据库**: MaxMind GeoIP
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
npm install
```

### 开发模式

```bash
npm run desktop
```

### 构建应用

```bash
npm run desktop:build
```

构建产物位于：

```text
src-tauri/target/release/bundle/
```

### 清理构建缓存

```bash
npm run clean:target      # 清理旧二进制和增量缓存
npm run clean:deep        # 深度清理（包括 .o 文件）
```

## 快捷键

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

应用内置以下资源：
- Mihomo 内核二进制
- GeoIP 数据库 (Country.mmdb)
- 本地化资源 (zh-Hans.lproj)

首次启动时会自动释放到应用数据目录。

## 轻量模式

应用支持轻量模式，通过本地 HTTP 服务提供浏览器访问：
- 启动本地服务器（默认端口 1420）
- 支持浏览器直接访问内核功能
- 可通过菜单切换桌面窗口

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
- 轻量模式浏览器访问

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
