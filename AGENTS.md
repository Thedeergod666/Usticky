# Usticky 项目说明

> 任何新打开此项目的 AI 会话应先读这个文件。当前快照：**v0.1 骨架 + 设置面板 + 三档 pin mode** / 2026-07-06。

## 这是什么

**Usticky** = "**U** + **sticky**"（"给你的 sticky note"）。常驻桌面浮窗的单人 todo 工具。

- **形态**：浮窗 + 系统托盘，无边框透明，玻璃质感，idle 全白 / hover 显彩
- **数据**：纯本地 `todos.json`（0600），**不联网**，**不同步**
- **快捷键**：`CmdOrCtrl+Shift+Space` 全局唤出快速添加
- **承诺**：3 秒唤出 + 写下 + 收起，不打断你的心流

## 技术栈（已拍板，照搬 Musage v0.2 决策）

| 层 | 选型 | 备注 |
|---|---|---|
| 框架 | **Tauri 2.x** | 同 Musage |
| 后端 | Rust (stable) | GNU 工具链（MinGW on Windows）|
| 前端 | Vanilla TypeScript + Vite 5 | 无 React/Vue，极小启动 |
| 持久化 | 本地 JSON + 原子写 | 沿用 Musage 范式（`tmp` → `rename` + 0600） |
| 拖拽 | SortableJS | 自己写 DnD 边界是噩梦 |
| 快捷键 | tauri-plugin-global-shortcut | 不抢 Spotlight/Cmd+Tab |
| i18n | 双 locale（en + zh-CN），前端自写 helper + 后端 rust-i18n | 沿用 Musage 架构 |
| 系统通知 | tauri-plugin-notification | v2+ 用（提醒临近 deadline） |

**Cargo 钉死**（避免重复踩坑）：
- `crate-type = ["staticlib", "rlib"]` —— 删 cdylib 绕 MinGW ld 16-bit ordinal 溢出
- `tauri` feature = `["tray-icon", "image-png", "macos-private-api"]`（macOS-private-api 是 entitlements 前置）
- `[profile.release]` `panic = "abort"` + `lto = true` + `opt-level = "s"`，**不**开 `strip = true`（与 rust-i18n 冲突）

**版本号钉死**（沿用 Musage 的 2026-06 实测稳态）：
- `@tauri-apps/api` / `@tauri-apps/cli` ^2.0.0
- `@types/node` ^20.0.0
- `typescript` ^5.6.0
- `vite` ^5.4.0
- `rustc` ≥ 1.77（edition = "2021"）

## 复用的 Musage 经验（"代码级"复用）

不是"概念上能用"，是**真的能复制粘贴**。Musage 项目位置：`~/Project/Musage/`。

| Musage 文件 | 复用到 Usticky 做什么 |
|---|---|
| `src/main.ts` 的 `render` / `buildCardSkeleton` / `updateCard` | 改写为 `renderTodos` / `buildTodoSkeleton` / `updateTodo`（diff 思路完全一样） |
| `src/main.ts` 的 `contentFingerprint` + `autoResizeWindow` | **直接抄**（用 `#app.scrollHeight` 不是 `documentElement.scrollHeight`） |
| `src/main.ts` 的 `rowKey`（kind-based，与 locale 解耦） | **直接抄** —— Usticky 用 `status:priority:tag` 做 key |
| `src/styles.css` 玻璃质感 + 省电模式 + iOS 26 widget | **整段复制**，`.card` 改名 `.todo-card` |
| `src/styles.css` 的 `.mini-flash` | **直接抄** |
| `src/main.ts` 的 `lastGoodSnap` + `TRANSIENT_ERROR_KINDS` | **不需要**（todo 没有"瞬态错误"概念） |
| `src-tauri/src/lib.rs` 的 `WindowEvent::Moved/Resized` 持久化 | **直接抄** —— spawn 异步任务，**不**在 UI 线程 blocking_write |
| `src-tauri/src/commands/mod.rs` 的 `reset_floating_window` / `resize_floating_window` | **直接抄** |
| `src-tauri/src/platform/macos.rs` 的 PinBottom + hover emitter | **已做（v0.1.2）** —— 三档 pin mode（PinTop/PinBottom/Normal，默认 PinBottom）+ 50ms tick hover emitter（`NSEvent.mouseLocation` + `windowNumberAtPoint` 命中测试）。Win 端 best-effort 实现（`HWND_BOTTOM`/`TOPMOST` dual-path），Linux no-op stub |
| `src-tauri/tauri.conf.json` 的 CSP / 浮窗 windows 配置 | **整段抄**，改 label / productName |
| `src-tauri/capabilities/` 的拆分模式 | **抄** —— 浮窗 capabilities vs 全局 capabilities 分开 |
| `src-tauri/entitlements.plist` | **整段抄**（Usticky 不联网，可以把 `network.client` 删掉，留 `network.server` 给未来 updater） |
| AGENTS.md 里 18 条浮窗经验 | **直接抄**到本文档第 3 节 |

**不借用 Musage 的**：
- 11 provider / QuotaSource trait / extra instance
- poller / backoff
- tray 动态图标进度条 / 双行百分比（Usticky 换成"任务总数 badge"）
- Xiaomi 一键登录 / Claude cookie
- api.rs / providers/* / schema 解析

## Musage 浮窗 18 条经验（直接抄过来的精简版）

这一节是"其他项目做浮窗前先读这个"。

### 1. 窗口行为（tauri.conf.json）

```jsonc
{
  "label": "floating",
  "decorations": false, "transparent": true,
  "alwaysOnTop": false,    // 默认 false，让用户选 pin_top / pin_bottom / normal
  "skipTaskbar": true, "shadow": false,
  "resizable": true,        // 即便"内容自适应"也要可拖
  "minWidth": 180, "minHeight": 160,
  "maxWidth": 420, "maxHeight": 2400
}
```

### 2. CSP —— 隐形雷区

```
default-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline';
script-src 'self'; connect-src 'self' ipc:; font-src 'self' data:;
frame-ancestors 'none'
```

- `img-src data:` 必加：fallback logo 用 `data:image/svg+xml,...`
- `connect-src ipc:` 必加：Tauri IPC 走 `ipc://` scheme
- 配套 Vite `assetsInlineLimit: 0`（<4KB 资源被内联 → CSP block → 裂图）

### 3. 渲染：增量 DOM diff，绝不 `innerHTML = ...` 全量替换

`innerHTML = ...` 会让整窗空白 1 帧 → "闪一下"。每张卡 / 每行用 `data-*` key 做增量 update。顺序变化：先按期望顺序插入 + reorder 循环搬已有卡，**快速路径：先比 expected/actual 字符串，相等就跳过整个循环**。

### 4. 内容高度自适应（防"浮窗越长越高"）

- 用 `#app.scrollHeight`，**不**用 `documentElement.scrollHeight`（后者陷入反馈环，几小时涨几十像素）
- `contentFingerprint` 去重：只看结构维度（卡数/行数/错误态），不看 utilization 数字 → 数据刷新不动尺寸，保留用户手动改的窗口高度

### 5. 位置 / 尺寸自动记忆

监听 `WindowEvent::Moved/Resized` → spawn 异步任务持久化。**不**在 UI 线程 blocking_write（卡渲染）。启动时在 `show()` 之前恢复 `last (x, y, w, h)`。

**配套提供"归位到主屏幕正中央"按钮**（设置面板里），用户换显示器 / 接副屏时一键回正。

### 6. macOS PinBottom 模式 = 私有 API

macOS 上 `set_always_on_top(false)` 不够 —— 窗口变 `kCGNormalWindowLevel = 0`，前台 app 调度直接埋掉。**用 objc2 直接调 `NSWindow.setLevel(-1)`**。

但 level -1 时 JS `mouseenter` 触发不到（WKWebView 在非 key window 不分发 mouseMoved）。解法：Rust 端 background thread 轮询 `NSEvent.mouseLocation()` + 窗口 `frame` 做 point-in-rect，emit `musage://floating-hover` 事件给前端 toggle `body[data-hover]`。

**Win 上做不到稳定 hover-raise** —— Win32 z-order 是平铺列表，OS 焦点调度持续 demote。Win 端 best-effort 实现（`HWND_BOTTOM`/`TOPMOST` dual-path + `SetWindowLongPtrW` 改 `WS_EX_TOPMOST` style bit + `GetAncestor(GA_ROOT)` 命中测试，详见 [platform/windows.rs](file:///Users/wyh/Project/Usticky/src-tauri/src/platform/windows.rs)）。Linux no-op stub（`set_always_on_top(true)` 已是最实用方案）。

**Usticky 决策**：v0.1.2 已实现三档 pin mode（PinTop / PinBottom / Normal，**默认 PinBottom**）。hover 临时置顶走 `NSWindow.setLevel` + `NSEvent.mouseLocation` 全局轮询 + `windowNumberAtPoint` 命中测试（详见 [platform/macos.rs](file:///Users/wyh/Project/Usticky/src-tauri/src/platform/macos.rs)）。dwell-time hysteresis（enter 3 ticks / exit 2 ticks）防边缘抖动振荡。

### 7. iOS 26 玻璃质感 / 待机省电双模式

```css
/* idle: 全部白，仿 macOS 26 天气 widget */
#app { --c-data-ok: white; --c-data-warn: white; ... }
/* hover: 唤醒 iOS 语义色 */
body[data-hover] #app { --c-data-ok: #30d158; ... }
/* 省电模式: 关 backdrop-filter + transition */
body[data-low-power] * { transition: none !important; backdrop-filter: none !important; }
```

色彩切换全部走 CSS variable swap，单一 `body[data-hover]` 触发整组变化，单一 ~280ms cubic-bezier 过渡。用户自定义色：JS 写 inline `app.style.setProperty('--c-data-ok', '#xxx')`。

### 8. 首启空态：别显示"⏳ Loading..."

检测"空列表" → 直接展示引导页 + 大 CTA "添加第一个任务"。

### 9. 错误处理：分层（瞬态 vs 持久）

todo 没有"瞬态错误"概念（不联网），但**仍**要分类：
- **用户操作错误**（输入为空 / 标题过长）→ 输入框本地校验，不入 IPC
- **存储错误**（磁盘满 / 权限丢）→ 浮窗闪红 + "查看日志" 按钮
- **未知错误** → 浮窗闪红 + 错误信息可复制

### 10. 倒计时：每秒 tick 走 data attribute

deadline 倒计时（"距离截止还有 2h15m"）每秒只改那一行 `.row-foot` 的 textContent，**绝不**每秒 render() 整张 snap → 整窗重建 → 巨卡。

### 11. 浮窗拖动：左键 mousedown

```ts
app.addEventListener('mousedown', (e) => {
  if (e.button !== 0) return;       // 仅响应左键
  if (e.target.closest('button, input, select, a, .todo-row')) return;  // 按钮 + todo 行不触发窗拖
  e.preventDefault();
  w.startDragging();
});
```

**Usticky 特别要小心**：拖动整个窗口 vs 拖动 todo 行的冲突 —— 必须在 mousedown target 检查 `.todo-row`。

### 12. 关闭 = 隐藏（不退出 app）

```rust
WindowEvent::CloseRequested { api, .. } => {
    api.prevent_close();   // 点 X 不退出，浮窗进 hide 状态
    // tray 左键单击 = 切换显隐
}
```

### 13. IPC 监听必须有 `.catch()` + beforeunload 清理

```ts
listen('usticky://todos-changed', handler)
  .then(fn => unlisten = fn)
  .catch(e => console.error(e));
window.addEventListener('beforeunload', () => unlisten?.());
```

### 14. 跨 webview 同步

设置面板改配置 → 浮窗即时生效：
```ts
listen('usticky://pin-mode-changed', async () => {
  const cfg = await invoke('get_pin_mode');
  // ... 重新设置 pin 控件 active 态
});
```

**不**走 `get_snapshot` + `render` —— 后端每次 IPC 都会 emit，自己 + 事件会 render 两遍 → 闪烁。**用 `lastRenderedSnap` 缓存**直接 render。

**✅ v0.1.2 已实现**：浮窗 [main.ts](file:///Users/wyh/Project/Usticky/src/main.ts) + 设置面板 [settings.ts](file:///Users/wyh/Project/Usticky/src/settings.ts) 都监听 `usticky://pin-mode-changed` / `usticky://locale-changed`，后端 `set_pin_mode_core` / `set_app_locale` emit。tray 子菜单的 checkmark 由 [lib.rs](file:///Users/wyh/Project/Usticky/src-tauri/src/lib.rs) 的 listener 调 `tray::rebuild_tray` 刷新。

### 15. locale 切换链路

```
前端 setLocale → invoke('set_app_locale') → Rust rust_i18n::set_locale + cfg 持久化
→ emit 'usticky://locale-changed' → 所有 webview 重建 dict → 重建 META + 刷名称
```

**单一来源 = 后端 locales/{en,zh-CN}.json**。前端别再镜像一份。

### 16. i18n JSON 双引号坑

中文里写 `"已内置"` 会提前结束 string。**用全角引号 `『』` 或 `\"` 转义**。

### 17. iOS 玻璃 + 暗色背景对比度

idle 白色数据 + 半透深底（`rgba(22,24,30,0.30)`）+ `backdrop-filter: blur(10px) saturate(140%)`。白色在深底上 ≥ 4.5:1 对比度稳过 WCAG AA。**hover 才上色** → idle 不色彩轰炸。

### 18. todo 浮窗的额外规则（v0.1 已采用）

- **输入中禁止 autoResizeWindow**（输入时 #app.scrollHeight 跳变 → 窗口抖）
- **拖拽完成立即乐观更新 DOM + 后台异步持久化**（不等 IPC 完成，避免感知延迟）
- **撤销栈最多 50 条**（避免无限增长）
- **快捷键不抢系统**：`CmdOrCtrl+Shift+Space` 是跟 Raycast 错开的安全位

## v0.1 当前状态

### v0.1.0 骨架（2026-07-02）

✅ 项目目录 + git init
✅ Tauri 2 配置文件（package.json / Cargo.toml / tauri.conf.json / capabilities）
✅ Vite 配置（port 1421 + assetsInlineLimit: 0）
✅ 前端骨架（main.ts / styles.css / i18n / index.html）
✅ 后端骨架（lib.rs / main.rs / todo.rs / commands / tray / platform）
✅ i18n 字典（en + zh-CN，前端 dict 已覆盖空态 / 输入 / due 标签 / 设置面板 / tray 全文案）
✅ 占位 icon
✅ 全局快捷键接线（CmdOrCtrl+Shift+Space → quick-add → 聚焦 input）

### v0.1.1（2026-07-02，搬 Musage 三档 pin mode）

✅ `todo.rs` `PinMode` enum（PinTop / PinBottom / Normal）+ 持久化到 `todos.json`
✅ `platform/macos.rs`：`NSWindow.setLevel` 切三档（`kCGFloatingWindowLevel` / `kCGNormalWindowLevel - 1` / `kCGNormalWindowLevel`）
✅ `platform/windows.rs`：`HWND_TOPMOST` / `HWND_BOTTOM` / `HWND_NOTOPMOST` dual-path（`SetWindowPos` + `SetWindowLongPtrW` 改 `WS_EX_TOPMOST`）
✅ `platform/mod.rs`：跨平台统一 API + Linux no-op stub

### v0.1.2（2026-07-03 → 2026-07-06，hover emitter + 设置面板 + tray 子菜单）

✅ Hover emitter（50ms tick，macOS `NSEvent.mouseLocation` + `windowNumberAtPoint` 命中测试；Win `GetCursorPos` + `WindowFromPoint` + `GetAncestor(GA_ROOT)`）
✅ Hover dwell-time hysteresis（enter 3 ticks / exit 2 ticks，防边缘抖动振荡）
✅ Hover 双路径（Rust emit `usticky://floating-hover` + JS `mouseenter`/`mouseleave` 40ms debounce）—— 失焦时主动 `setHoverAttr(false)` 清 stale state
✅ SortableJS 拖拽排序（pending / done section 各一个 Sortable 实例，`onEnd` 批量 `reorder_todos`）
✅ 标记完成动画（`.vanishing` class + 300ms 延迟后才调 IPC，失败回滚 class）
✅ 设置面板（[settings.html](file:///Users/wyh/Project/Usticky/settings.html) + [src/settings.ts](file:///Users/wyh/Project/Usticky/src/settings.ts) + [src/settings.css](file:///Users/wyh/Project/Usticky/src/settings.css)）：单页设计，pin mode segmented control + 语言切换 + 浮窗归位 + 关于
✅ `open_settings_window` 命令（动态创建 webview，已开则 focus，关闭时 destroy，不在 tauri.conf.json 常驻）
✅ Tray Settings 子菜单（pin mode 三档 `CheckMenuItem` + "Open Settings Panel..."）—— locale / pin mode 切换时 `rebuild_tray` 走 `run_on_main_thread` 派发避免 NSStatusBar SIGTRAP
✅ Tray icon 改 U 字母（[scripts/generate_icons.py](file:///Users/wyh/Project/Usticky/scripts/generate_icons.py)：白底圆角 + 黑色加粗 U + ring 装饰，每个尺寸原生渲染，macOS 用 `iconutil` 拼真 .icns）
✅ `reset_floating_window` / `resize_floating_window` / `hide_floating_window` / `show_floating_window` 命令
✅ `set_floating_hover_raise` 命令（前端兜底信号，macOS/Win 上 tracker 已自行处理，此处 no-op）
✅ locale 切换链路：tray + settings 窗口 title 同步重建（`usticky://locale-changed` listener）
✅ Pin mode 跨 webview 同步（`usticky://pin-mode-changed` listener 在浮窗 / 设置面板 / tray 三处生效）
✅ `persist_and_emit` 失败时 emit `usticky://persist-failed`（不再静默吞掉，前端 mini-flash 提示）

### 仍未做

⏳ **Cmd+Z 撤销栈**（[main.ts](file:///Users/wyh/Project/Usticky/src/main.ts) 已占位 keydown listener，TODO 未实现，v0.2 候选）
⏳ **tray 图标任务数 badge**（v0.1 是静态图标 `tray-base.png`，v0.2 候选）

## v0.2 候选

| Feature | 价值 | 复杂度 |
|---|---|---|
| Cmd+Z 撤销栈（最多 50 条） | 必备 | ⭐⭐ |
| 全局快捷键冲突检测 | 必备 | ⭐ |
| 全文搜索（Cmd+F 浮窗内） | 列表长时必备 | ⭐⭐ |
| tray 图标任务总数 badge | 锦上添花 | ⭐⭐ |
| 提醒通知（tauri-plugin-notification） | 临近 deadline 弹 | ⭐⭐ |
| 标签分组（折叠） | 工作流成熟后 | ⭐⭐ |
| iCloud 同步（CloudKit） | 多设备 | ⭐⭐⭐ |

## 已知坑（来自 Musage 同款决策）

详见 `docs/quirks.md`（**尚未整理** —— v0.1.2 hover emitter / PinBottom 调试期间的多条 fix 散落在 [platform/macos.rs](file:///Users/wyh/Project/Usticky/src-tauri/src/platform/macos.rs) / [platform/windows.rs](file:///Users/wyh/Project/Usticky/src-tauri/src/platform/windows.rs) / [main.ts](file:///Users/wyh/Project/Usticky/src/main.ts) 的内联注释里，待 v0.2 阶段统一归纳）。

## 文件结构

```
~/Project/Usticky/
├── AGENTS.md                 ← 本文件（项目交接文档）
├── README.md
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
│   ├── assets.d.ts
│   └── i18n/
│       ├── index.ts          ← 前端 i18n helper（locale 持久化 + onLocaleChange）
│       ├── en.json           ← 前端 dict（dotted key，覆盖空态/due/设置/tray）
│       └── zh-CN.json
└── src-tauri/
    ├── Cargo.toml
    ├── tauri.conf.json       ← 浮窗 windows 配置（只声明 floating，settings 动态建）
    ├── build.rs
    ├── entitlements.plist
    ├── capabilities/
    │   ├── default.json      ← 浮窗 capabilities
    │   └── global.json       ← 全局 IPC capabilities
    ├── icons/                ← generate_icons.py 产物（含 tray-base.png）
    ├── locales/              ← en.json + zh-CN.json（rust-i18n 后端单一来源）
    └── src/
        ├── main.rs           ← Windows / Linux 入口
        ├── lib.rs            ← Tauri Builder + 快捷键 + 窗口事件持久化 + locale/pin mode listener
        ├── todo.rs           ← Todo + PinMode + StoreData + JSON storage（原子写 + 0600 + .bak）
        ├── tray.rs           ← 系统托盘（Settings 子菜单 + pin mode checkmark + rebuild_tray）
        ├── commands/
        │   └── mod.rs        ← CRUD + 浮窗控制 + i18n + pin mode + open_settings_window
        └── platform/
            ├── mod.rs        ← 跨平台统一 API（pub use plat::*）
            ├── macos.rs      ← PinBottom/PinTop/Normal + hover emitter（已实现）
            ├── windows.rs    ← HWND_TOPMOST/BOTTOM dual-path + hover emitter（best-effort）
            └── (linux)       ← mod.rs 内 no-op stub（无 linux.rs 文件）
```