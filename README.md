# Usticky

> Always-on-top floating sticky note for desktop. Single-task focus, glass aesthetic, no bloat.

## Why Usticky

- **One window, one purpose**: 浮窗始终在桌面一角，唤出即写、收起即走
- **零学习成本**: 没有项目 / 标签 / 嵌套层级，没有 due date 排程 —— 就是便签的数字化
- **不联网**: 所有 todo 存本地 JSON（0600 权限），你的脑子不需要云同步
- **iOS 26 玻璃质感**: 借鉴 macOS Sonoma widget，idle 全白、hover 才显彩，**不抢注意力**

## Tech stack

| 层 | 选型 |
|---|---|
| 框架 | **Tauri 2.x** |
| 后端 | Rust (stable) |
| 前端 | Vanilla TypeScript + Vite（无框架，极小） |
| 持久化 | 本地 `todos.json`（Unix 0600 权限，原子写） |
| 拖拽 | SortableJS |
| 快捷键 | tauri-plugin-global-shortcut |
| i18n | 自写 helper 前端 + rust-i18n 后端（en + zh-CN） |

## Quick start

```bash
pnpm install
pnpm tauri:dev
```

构建：

```bash
pnpm tauri:build          # macOS dmg + Windows NSIS
```

## Project layout

```
Usticky/
├── AGENTS.md                 ← 项目交接文档（先读这个）
├── README.md                 ← 本文件
├── CHANGELOG.md
├── package.json / pnpm-lock.yaml
├── tsconfig.json / vite.config.ts
├── scripts/
│   ├── sync-version.cjs      ← 三处 version 同步
│   └── generate_icons.py     ← U 字母 icon 生成（PNG/ICO/ICNS + tray-base.png）
├── index.html                ← 浮窗入口
├── settings.html             ← 设置面板入口（动态创建 webview，非常驻）
├── src/
│   ├── main.ts               ← 浮窗：渲染 + 拖拽 + 输入 + 快捷键 + hover 双路径
│   ├── styles.css            ← iOS 26 玻璃质感（沿用 Musage）
│   ├── settings.ts           ← 设置面板：pin mode + 语言 + 归位 + 关于
│   ├── settings.css          ← 设置面板样式
│   ├── assets.d.ts           ← ?url 模块声明
│   └── i18n/                 ← en.json + zh-CN.json + index.ts
└── src-tauri/
    ├── Cargo.toml
    ├── tauri.conf.json       ← bundle targets: nsis + dmg；windows 只声明 floating
    ├── build.rs / entitlements.plist
    ├── capabilities/
    ├── icons/                ← generate_icons.py 产物（含 tray-base.png）
    ├── locales/              ← en.json + zh-CN.json（rust-i18n 后端单一来源）
    └── src/
        ├── main.rs           ← Windows / Linux 入口
        ├── lib.rs            ← Tauri Builder + 快捷键 + 窗口事件持久化 + locale/pin mode listener
        ├── todo.rs           ← Todo + PinMode + StoreData + JSON storage（原子写 + 0600 + .bak）
        ├── tray.rs           ← 系统托盘（Settings 子菜单 + pin mode checkmark）
        ├── commands/mod.rs   ← CRUD + 浮窗控制 + i18n + pin mode + open_settings_window
        └── platform/         ← macOS NSWindow.setLevel + hover emitter / Win HWND dual-path / Linux no-op
```

## Relationship to Musage

Usticky 直接借用 [Musage](~/Project/Musage) 的浮窗经验（详见 [AGENTS.md](AGENTS.md) 第 1 节"复用的 Musage 经验"）：

| Musage 概念 | 在 Usticky 的形态 |
|---|---|
| 11 个 provider quota 监控 | → 单用户的本地 todo list |
| API key 存 `keys.json` | → todo 存 `todos.json`（同款原子写 + 0600） |
| 增量 DOM diff 渲染 | → 直接复制 `render` / `buildCardSkeleton` 改写为 todo 行 |
| `#app.scrollHeight` 自适应 + contentFingerprint | → 直接复制 |
| 浮窗位置/尺寸自动记忆 | → 直接复制 |
| iOS 26 玻璃 CSS + 省电模式 | → 直接复制 + 改类名 |
| i18n 双 locale 架构 | → 直接复制（en + zh-CN） |
| PinBottom 私有 API + hover emitter | → **v0.1.2 已搬**（三档 pin mode + 50ms tick 全局鼠标轮询） |

不借用 Musage 的：11 provider / API 轮询 / tray 动态进度条（Usticky tray 是静态图标，任务总数 badge 留待 v0.2）。

## License

个人项目，未指定。