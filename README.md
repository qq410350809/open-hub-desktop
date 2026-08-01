# OpenHub

使用 Tauri 2 + SQLite 构建的本地站点资料库。

## 产品定位

这不是线上网站套壳，也不是远程服务状态导航。应用启动后直接进入本地资料库，站点记录只保存在当前设备。

## 功能

- 新增、编辑和删除站点记录
- 保存站点名称、URL、分类、标签和备注
- 收藏常用站点
- 按关键词、分类和收藏状态筛选
- 调用系统默认浏览器打开站点
- 明暗主题
- SQLite 本地持久化
- 不需要登录
- 不请求远程站点数据

## 数据库位置

应用第一次启动时会在 Tauri 的应用数据目录创建：

```text
sites.sqlite3
```

macOS 通常位于：

```text
~/Library/Application Support/com.dfeer.openhub.desktop/sites.sqlite3
```

数据库启用了 WAL 模式，运行期间还可能出现 `sites.sqlite3-wal` 和 `sites.sqlite3-shm` 文件，这是 SQLite 的正常行为。

## 开发

```bash
npm install
npm run desktop
```

## 构建

```bash
npm run desktop:build
```

构建产物位于：

```text
src-tauri/target/release/bundle/
```

## 数据结构

`sites` 表主要字段：

- `id`
- `name`
- `url`
- `category`
- `description`
- `tags`（JSON 字符串）
- `favorite`
- `created_at`
- `updated_at`

macOS 当前使用 ad-hoc 签名用于本地测试。公开分发时应配置 Developer ID 并完成 notarization。
