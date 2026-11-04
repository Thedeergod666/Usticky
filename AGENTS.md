# Usticky 项目说明

> 任何新打开此项目的 AI 会话应先读这个文件。当前快照：**v0.2.10（玻璃心跳从「每卡一个」降为「窗口一个」哨兵）/ 2026-11-03 —— ⚠️ 在分支 `perf/window-keepalive-glass`，未合入 main，待用户视觉验收**。合入前请读 v0.2.10 段的验收清单。上一版已合入 main 的快照：**v0.2.9（换行丢失根修：编辑态 textarea 化 + 多行粘贴走全文路径）/ 2026-10-27**。

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
| 系统通知 | tauri-plugin-notification | v0.2+ 用（提醒临近 deadline），v0.1 不依赖 |

**Cargo 钉死**（避免重复踩坑）：
- `crate-type = ["rlib"]` —— 删 staticlib 绕 MinGW ld 16-bit ordinal 溢出（v0.1.5 P3-4 删）
- `tauri` feature = `["tray-icon", "image-png", "macos-private-api"]`（macOS-private-api 是 entitlements 前置）
- `[profile.release]` `panic = "abort"` + `lto = true` + `opt-level = "s"`，**不**开 `strip = true`（与 rust-i18n 冲突）

**版本号钉死**（沿用 Musage 的 2026-06 实测稳态）：
- `@tauri-apps/api` / `@tauri-apps/cli` ^2.0.0
- `@types/node` ^20.0.0
- `typescript` ^5.6.0
- `vite` ^5.4.0
- `rustc` ≥ 1.84（edition = "2021"；u64::is_multiple_of 在 1.84.0 stable，2025-01-09）

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
| `src-tauri/entitlements.plist` | **整段抄**（Usticky 不联网——零 HTTP / 零 fetch / 零 IPC 之外的 connect——Hardened Runtime 不加任何 network entitlement，最小攻击面） |
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
  "minWidth": 240, "minHeight": 160,
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

### 3. 渲染：已是 keyed diff 增量渲染（v0.2.7 起；v0.2.8 补多值 Map 防御）

**stale claim 修正（v0.2.8）**：本节旧版写"当前为全量重建，未来迁移增量 diff" —— 实际 commit `b628d1f`（2026-08-11）已完成迁移，此后 PERF_AUDIT.md 的 S1/S2/S3/A1/A2/A3/B2/B3/B4 系列持续打磨。现状：`render()` 按 section 走 `diffSection` keyed diff（`data-todo-id` 复用行节点、`buildTodoRow` 零监听器骨架 + `updateTodoRow` 原地更新、事件全部委托到 `#app` 3 个 listener、expected/actual 字符串快速路径跳过 reorder 循环、Sortable 实例随 section 跨 render 复用）。v0.2.8 再补：diffSection 多值 Map（Musage 同款重复 key 防御）、updateTodoRow 同值守卫、hidden 窗口渲染门。

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
✅ i18n 字典（en + zh-CN，前端 dict 已覆盖空态 / 输入 / due 标签 / 设置面板 + 后端 locales 覆盖 tray 全文案）
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

### v0.1.3（2026-07-21，未聚焦卡片 hover 修复 + 复制按钮 + 删除二次确认）

✅ **hover-pos 改发视口相对坐标（根因修复）**：旧实现 Rust 发屏幕坐标、前端用 `innerPosition()` + `window.screen.height` 手工换算 —— 叠了 tao `bottom_left_to_top_left` physical/logical 单位混用、`window.screen` 是窗口所在屏而 `mouseLocation` 以主屏为基准、Retina scale 三层易碎假设。实测同一公式在不同机器/显示器配置下换算结果不同（85dc58a 修副屏反而打破主屏，relY 算出 1204px 远超 703px 窗口 → `elementFromPoint` 恒 null → 未聚焦 hover 卡片永远不展开、删除按钮（纯 CSS `:hover`）永不显示）。**修复：换算挪到 Rust 端 —— `NSEvent.mouseLocation` / `GetCursorPos` 与窗口 frame 同坐标系直接相减（macOS 用窗口自身高度翻 Y 轴，Win 除 scale），前端拿到直接用，零假设**。规则：**永远别让前端做屏幕坐标→视口坐标换算**。
✅ 前端 hover-pos listener 不再要 `rustPathActive` 开关（hover-pos 只在 Rust inside=true 时发送，收到即激活，vite HMR 刷新后自愈）
✅ `.card-hover` class 驱动卡内按钮显隐：未聚焦窗口 WKWebView 不激活 CSS `:hover`，Rust hover-pos 命中的卡片挂 `.card-hover`，CSS `.todo-card:hover` 与 `.todo-card.card-hover` 双条件显示操作按钮
✅ 复制按钮（删除键左侧，`navigator.clipboard.writeText` + `execCommand` 兜底，mini-flash 反馈）
✅ 删除二次确认：第一次点击进入确认态（实红 + `data-confirm="1"`），3s 内第二次点击才真删；超时 / hover 结束（`unhoverCard` / `mouseleave`）自动撤销 —— 防"按钮已隐藏但确认态还在"的误删
✅ 新增 i18n key：`app.action.copy` / `app.action.confirm_delete` / `app.copy.flash` / `app.error.copy_failed`

### v0.1.4（2026-07-21，未聚焦按钮交互 + 单击直达 + × 残留修复）

✅ **按钮显隐/反馈彻底弃用 CSS `:hover`**：非 key window 的 WKWebView `:hover` 既不激活、又会在聚焦/失焦边界 **stuck 残留**（× 残留在多张卡上的根因）。改为单一状态机：`.card-hover` / `.btn-hover` class，由 JS `mouseenter/leave`（聚焦）+ Rust hover-pos `elementFromPoint` 命中（未聚焦）双路径驱动同一对 `hoverCard`/`unhoverCard`/`setBtnHover`。规则：**浮窗内一切 hover 驱动的样式，都不许用 `:hover`，走 class**
✅ 按钮 hover 反馈（背景变色）走 `.btn-hover`；手型光标走 `set_cursor_pointer` 命令 → macOS `NSCursor.pointingHandCursor().set()`（非 key window WKWebView 不更新光标；Win 上悬停窗口自收 `WM_SETCURSOR`，no-op）
✅ **`acceptFirstMouse: true`（tauri.conf.json）**：未聚焦时点一次复制键即触发（旧行为第一击被 click-to-focus 吞掉，要点两次）
✅ 按钮压缩 + 不占位：`.todo-actions` 容器（gap 2px，按钮 22→18px），**默认 `display:none`**，`.card-hover` 才 `display:flex` —— 未 hover 时 title 不再被白挤 ~46px
✅ 实测（CGEvent warp + CGWindowList + 临时 IPC 信标）：穿梭 9 卡 × 3 往返 `.card-hover` 计数恒 ≤1，单击复制落剪贴板成功（`hasFocus:true` 说明 acceptFirstMouse 生效）

### v0.1.5（2026-07-29，全量代码审查修复 + CI 流水线 + single-instance）

✅ **`release.yml` tag pattern 修复**（P0-1）：原 `v[0-9]+.[0-9]+.[0-9]+*` 是 fnmatch glob，永不匹配 `v0.2.0`。改 `v*.*.*`，AGENTS.md 写的就是这个。
✅ **`workflow_dispatch` 版本同步修复**（P0-2）：原 inline Node 脚本写 `tauri.conf.json` 后调 `pnpm sync-version`，后者读 `package.json.version` 反向覆盖 → dispatch 版本被吞。改写 `package.json` + sync 推 tauri.conf.json + Cargo.toml。
✅ **`reorder` 防御性 guard**（P1-1）：front-end 发送 proper subset 时不静默错序，改 `Result<()>` + 拒绝非整段 section。
✅ **`persist_to_path` 释放 RwLock**（P1-2）：原 `&self` 持锁跨 fsync。重构为 free function `persist_to_disk(path, data)` + process-level `OnceLock<Mutex<()>>`，3 个调用点全改；新加 5 个 regression test。
✅ **`register_quick_add_shortcut` 顺序修复**（P1-3）：原 `unregister_all` 在 parse/on_shortcut 之前 → 失败时旧快捷键永久丢失。改：parse → on_shortcut 成功才 unregister；失败 best-effort 重新注册 previous。
✅ **`set_quick_add_shortcut` 持久化失败回滚**（P1-4）：原内存先写、磁盘后写 → persist 失败时用户以为改了。改：snapshot previous → 写 → persist 失败 → 回滚 + emit `persist-failed` + 不 re-register。
✅ **快捷键必须带 modifier**（P1-5）：原接受 `"F12"`、`"A"` 等单键 → 任何 app 按 F12 都触发 quick-add。改：parse 后 `mods.intersects(CONTROL|ALT|META|SUPER)` 否则 Err。
✅ **Quick-add 原子化**（P1-9 / P2-5）：原 check-then-act + 后置 `store(true)`，双击双跑。改 `compare_exchange(false, true, SeqCst, SeqCst)`。
✅ **macOS PinBottom hover 切换不闪烁**（P1-6）：原 FIFO 主线程队列先 LOWER 再 RAISE。改：悬停时 BELOW_NORMAL 延 50ms + 条件 skip（`LAST_INSIDE` 仍 true → 跳过 LOWER）。
✅ **Win z-order 锁**（P1-7）：emitter 线程与 main-thread pin-mode 切换可交错。改 `static APPLY_Z_ORDER_LOCK: Mutex<()>` 包 3 步 Win32 序列。
✅ **Win scale_cache 跨 DPI 屏刷新**（P1-8）：原 spawn 时取一次 scale forever。改 `Mutex<Option<f64>>` + 每 60 tick lazy refresh。
✅ **macOS dispatch_failed recovery 不无条件 LOWER**（P2-4）：原同 BUG-001 闪烁。改：只 emit `hover(false)`，下一 tick 自然恢复。
✅ **tauri-plugin-single-instance 集成**（P2-6）：双开 dock icon / 终端运行不再二次启动。`Cargo.toml:23` + `lib.rs:217-228` `.plugin(tauri_plugin_single_instance::init(...))`。
✅ **Tray menu 打开时跳过 rebuild**（P2-7）：原 detach 活跃 NSMenu。改 `MENU_OPEN_FLAG: AtomicBool` + Right Down/Up + on_menu_event 双向检测。
✅ **Moved/Resized 200ms trailing debounce**（P2-8）：原每像素 spawn 持久化。改 `GEOM_NOTIFY: OnceLock<Notify>` + 后台 `geom_persist_loop` select 模式。
✅ **tray::build_tray 改 `try_state`**（P2-9）：原 `app.state()` 可在退出时 panic。
✅ **`reset_floating_window` 用持久化 (w,h)**（P2-12）：原用 `outer_size()` live 尺寸，多屏切换后尺寸漂移。
✅ **`reset_floating_window` 检查 `is_visible()`**（P2-16）：隐藏时不要 `set_position`（macOS 会顺带浮起）。
✅ **`reset_floating_window` 持久化后 emit `usticky://window-pos-changed`**（P2-19）。
✅ **`set_app_locale` 白名单**（P2-3 / P2-20 / 多域同根）：`["en", "zh-CN"]` 否则 Err。新 i18n key `commands.error.unsupported_locale`。
✅ **`update_todo` no-op 短路**（P2-4）：`title=None && status=None` 早返 `Ok(None)`，跳过 persist + `updated_at` bump。
✅ **`persist_to_disk` 用 `OpenOptions::mode(0o600)`**（P2-5）：不再 chmod 失败后世界可读 tmp。
✅ **`persist_to_disk` rename 后 fsync 父目录**（P2-6）：断电后 rename 不丢。
✅ **Quick-add Enter 输入失败回填**（P1-9，前端）：原 `input.value = ""` 在 await 之前 → 失败时用户输入丢失。改 await 成功后才清空。
✅ **Settings locale double-render 修复**（P1-10）：原 `onLocaleChange` + Tauri listener 双 render。改：只靠 Tauri listener 路径。
✅ **Settings invoke 失败给 flash + retry**（P1-11）：原只 `console.error` 静默回退硬编码默认值。
✅ **快捷键录制 button blur 退出 recording**（P1-12）：原只能等 10s timer 或 Esc。
✅ **Format shortcut Win 显示 Ctrl 而非 Cmd**（P2-13）。
✅ **Click handler `e.button !== 0` skip 中右键**（P2-14）。
✅ **`closest` selector scope 到 `[data-pin]` / `[data-locale]` 容器**（P2-15）。
✅ **Locale 切换 render 保留 recording 状态**（P2-17）。
✅ **Locale listener 总是 setLocale**（P2-18）：不再 gate 在 `getLocale()` 缓存上。
✅ **beforeunload 提到 init() 顶部**（P2-19）。
✅ **语言按钮走 i18n**（P3-13）：`settings.language.option_en` / `option_zh-CN`。
✅ **`sortable onEnd` 失败 re-fetch + flash**（P2-10，前端）。
✅ **Esc 只在 quick-add 唤起时 hide 窗**（P2-11，前端）：tray 唤起窗按 Esc 仅 `input.blur()`。
✅ **`Cmd+Z` 占位 handler 删除**（P3-7，前端）：v0.2 再加。
✅ **icon-only copy/delete 按钮加 `aria-label`**（P3-5，前端）。
✅ **`setBtnHover` 缓存 `lastPointerState`**（P3-6，前端）：不变就不 invoke。
✅ **i18n 死 key 清理**（P3-16/17/18）：删后端 `error.empty_title` / `error.not_found` / `error.too_long`；前端 `tray.show` / `tray.hide` / `app.action.edit` / `app.empty.done`。
✅ **`commands.error.too_long` 用 `{max}` 占位**（P2-23）：`app.error.too_long` 同步。
✅ **`due.future` 改小写对齐 `due.days`**（P3-19）。
✅ **CI 改 `cargo check`（去 `--locked`）**（P1-13）：`prebuild` 改 `Cargo.toml` 后 lockfile 同步失败误导。
✅ **release.yml Sync version 移到 `pnpm install` 之后**（P1-14）。
✅ **rust-cache 配 `cache-on-failure: false`**（P2-25）。
✅ **Verify job 改 polling 循环**（P2-26）：取代 `sleep 15`。
✅ **`pnpm icons` script 补回**（P2-27）+ `requirements.txt` 加 Pillow 依赖声明。
✅ **删未用 `plugin-notification` / `plugin-autostart` JS 依赖**（P3-1）。
✅ **capabilities 裁剪到最小集**（P3-2/3）：删冗余 `opener:default` + 7 个未调用的 `core:window:allow-*`。
✅ **`staticlib` crate-type 删**（P3-4）：CI 是 MSVC，desktop 用 rlib 足够。
✅ **`Cargo.toml:14` 改 `crate-type = ["rlib"]` 并加注释**。
✅ **AGENTS.md 文档修正**：v0.1.2 的 "P3-4 fix 不持锁 I/O" 描述是 stale claim（实际仍持锁；v0.1.5 才真正释放）；`render()` 实际是全量重建（非 "incremental DOM diff" 描述）；前端 `tray 全文案` claim 过期（实际 `tray.show`/`tray.hide` 是死 key，由后端 locales 覆盖）。

### v0.2.0（2026-08-04，剪贴板粘贴 + QuickLook 预览窗）

✅ **粘贴按钮**（输入行最右侧，`.todo-paste-btn`）：`paste_from_clipboard` 命令统一入口 —— 剪贴板是文本 → 整段作为一个 pending todo（多行保留，长文靠预览窗看全文）；是图片 → 落盘 `<data_dir>/attachments/<uuid>.<ext>` + 带 attachment 的 todo；空 → Err("empty") 前端 flash。Rust 端读剪贴板（tauri-plugin-clipboard-manager）无焦点要求，**不走**前端 `navigator.clipboard.read()`（非 key window 不可靠）。
✅ **GIF / 图片支持**（[clipboard.rs](file:///Users/wyh/Project/Usticky/src-tauri/src/clipboard.rs) 三条路径）：① macOS NSPasteboard 原始字节 —— `public.gif` 原数据**保动画**，`public.file-url` 支持 Finder 复制图片文件，`public.tiff` 走 image crate 转 PNG；② 插件 `read_image` RGBA → PNG 跨平台兜底（Win/Linux 主路径，**GIF 退化首帧**，已知限制）；③ 文本。体积上限 25MB。
✅ **Todo.attachment 字段**（`TodoAttachment { file, mime, width, height }`，serde default 向后兼容）：只存相对文件名，绝对路径运行时拼（`Store::attachments_dir()`）。delete_todo **不**立即删附件文件（v0.2.5 改延迟删除）：前端 undo 栈超时（8s）调 `purge_attachment` 真删 + 启动孤儿扫描（`Store::purge_orphan_attachments`）兜底崩溃残留，撤销窗口内图片完整可恢复。
✅ **asset 协议显示**：tauri.conf.json `assetProtocol.scope = ["$APPDATA/attachments/**"]` + CSP `img-src` 加 `asset: http://asset.localhost`（Tauri 2 没有 core:asset 权限，纯 scope 配置驱动）。前端 `convertFileSrc(attachmentsDir + "/" + file)`，GIF 在 `<img>` 里原生动画。
✅ **QuickLook 预览窗**（[preview.html](file:///Users/wyh/Project/Usticky/preview.html) + [preview.ts](file:///Users/wyh/Project/Usticky/src/preview.ts) + [preview.css](file:///Users/wyh/Project/Usticky/src/preview.css)）：独立动态 webview（抄 open_settings_window 模式），无边框透明 + always-on-top + acceptFirstMouse，定位浮窗左侧优先（放不下放右侧），尺寸按附件宽高比算。**hover 长文不再变长（hover-expand 整套退役，scheduleHoverResize/.title-expanded 已删）**：截断 title dwell 600ms / 图片卡 350ms → `open_preview_window(pinned=false)` 非聚焦面板；点击缩略图 → `pinned=true` 聚焦编辑态。
✅ **预览窗可编辑**：textarea 防抖 700ms 自动保存（update_todo），空标题不保存；切 todo / 关窗前 flushPendingSave 强制落盘；外部 todos-changed 在 textarea 未聚焦时回填，todo 被删 → 自关。
✅ **跨 webview 生命周期**（浮窗 main.ts 预览状态机 + preview.ts 配对）：`previewTodoId` / `previewPinnedId` / dwell timer / 450ms grace close timer。鼠标从卡片滑向预览窗时 preview.ts emit `preview-entered` → 浮窗取消自动关（pinned）；离开未聚焦 → `preview-left` 重启 grace close；Esc / 窗口 blur 自关 + emit `preview-closed` 双端复位。**pinned 期间 hover 其他卡不换预览内容**（防编辑被抢）。浮窗 hide（hide_dismiss + CloseRequested 两条路径）连带关预览窗。
✅ **窗口复用**：open_preview_window 检测已开 → 不重建（避免 reload 丢编辑内容），emit `usticky://preview-todo` 原地换内容 + resize/移窗。position 是逻辑坐标（除 scale），set_position 是物理坐标 —— 别混。
✅ 新增 i18n key：前端 `app.action.paste/preview` + `app.paste.*` + `preview.*`；后端 `commands.paste.image_title` + `window.preview`。
✅ 构建验证：`pnpm build`（tsc + vite）✓、`cargo check --all-targets` 零警告 ✓、`cargo test --lib` 20/20 ✓。

**已知限制**：Windows 剪贴板读图只有 RGBA（GIF 变首帧 PNG）；Finder 文件粘贴仅 macOS 支持；预览窗 blur 自关 = 编辑中切走 app 会关窗（内容已自动保存，可接受）。

### v0.2.1（2026-08-05，hover 预览四问题修复）

用户实测反馈四个问题，根因 + 修法：

✅ **① 粘贴图片后 todo 内显示空白框 + 占用过高**：双层根因 -- a) **首启竞态**：`get_attachments_dir` 在 `init()` 里排在 `get_todos`/首次 `render()` **之后**（[main.ts](file:///Users/wyh/Project/Usticky/src/main.ts) 旧 ~1830 行），已存在附件的 todo 首屏 `buildTodoRow` 调 `attachmentUrl()` 时 `attachmentsDir` 仍 null -> `<img>` 无 `src` -> 空白边框框（且无 error 事件 -> `thumb.remove()` 不触发，框常驻）。修：`get_attachments_dir` 挪到 `get_todos` **之前**。b) **展示方式**：图片改**内联单行**（check 与 title 之间，`.todo-thumb`），`flex:1 1 0` 跟 `.todo-title` 1:1 各占一半宽 + `align-self:stretch` 填内容高（行高跟纯文本 todo 一致，保持不变）；纯图 todo（空标题）走 `.todo-title:empty`（`flex:0 0 auto`）折叠成 0 宽 -> 图片独占整宽。点击仍聚焦预览窗看全图 / GIF 动画。`.todo-card` 不用 flex-wrap（单行 nowrap）。`.todo-title` / `.todo-edit-input` flex 由 `1 1 auto` 改 `1 1 0`（跟图片 1:1 共享宽度）。`NON_DRAGGABLE_SELECTORS` 含 `.todo-thumb`。顺手删了缩略图上残留的 `title=`（违反 v0.2.1 "浮窗内不准原生 tooltip" 规则的漏网之鱼）。 hover 时图片不收缩：setCardHover 挂 .card-hover **前**把 .todo-thumb 当前宽（= 无 actions 的 1:1 宽）冻成 flex:0 0 <w>px -> actions 出现时只有 .todo-title 收缩省略（"..."），图片保持原宽（默认 1:1 不变，hover 不硬保持 1:1）。setCardHover 幂等，统一 hoverCard / unhoverCard / over_preview 三处 .card-hover 增删，避免漏解冻留 stale freeze。
✅ **② 有时显示"小弹窗"**：不是预览窗，是 `.todo-title` 的原生 `title=` tooltip（macOS hover 停留 ~1-2s 弹白色小条）。修：buildTodoRow / exitEditMode 两处 `title.title = ...` **已删**，长文预览统一走 QuickLook 预览窗。**规则：浮窗/预览窗内不准用原生 title= tooltip**。
✅ **③ 预览出现 ~2s 后丢毛玻璃**（gif 证实，连浮窗玻璃一起丢）：macOS WKWebView backdrop-filter sample 在新 always-on-top 窗口上屏后失效的旧病（v0.1.2 起浮窗靠 level 切换后 emit backdrop-refresh 救；预览窗开着期间没有 level 切换可搭车）。修：a) open/close preview 两条路径末尾 emit `usticky://backdrop-refresh` 救浮窗；b) preview.ts **heartbeat**（每 1200ms 给 .preview-panel 挂 100ms `.force-reflow`（`filter: drop-shadow(0 0 0 transparent)`，浮窗 styles.css 同款 paint invalidation），`visibilityState === "hidden"` 跳过 —— prewarm 隐藏态不耗 GPU）。
✅ **④ 鼠标停在大卡片上预览会消失**（振荡）：根因双层 —— a) 预览窗定位可能盖住浮窗/鼠标 → `windowNumberAtPoint` 命中预览窗而非浮窗 → hover emitter 报 inside=false → unhoverCard → 450ms grace close → 关窗 → 又 inside → 循环；b) 预览窗非聚焦态 WKWebView 不派 mouseenter → `preview-entered` 发不出 → pinned 机制失效。修：
  - **over_preview-inside**（macOS + Win 双端 hit test）：emitter 命中测试先看最上层是不是预览窗 —— 是 → `inside=true` + payload 加 `over_preview: true`（macOS 取 preview `windowNumber()` 比对 topmost；Win `WindowFromPoint` + `GetAncestor(GA_ROOT)` 比对 preview hwnd，**hwnd 每次现取不缓存** —— 窗口动态销毁重建会变）。前端 hover-pos listener 收到 `over_preview` → 预览 pinned + 取消 dwell/grace close + 浮窗卡片 unhover（先置 pinned 再 unhover，unhoverCard 的 pinned 守卫跳过关窗），**不跑 elementFromPoint**（坐标相对浮窗无意义）。
  - **floating-hover(false) 才关预览**：能收到 false 说明鼠标既不在浮窗也不在预览窗 → 清 previewTodoId/previewPinnedId + `close_preview_window({force:false})`。`force:false` = 预览窗正聚焦（编辑中）则不关，blur 后自治关闭。
  - **四向定位**（commands/mod.rs `preview_position()`）：左→右→上→下，第一个不与浮窗 rect 相交且在显示器内的方向胜出；兜底右侧 clamp 允许重叠（over_preview-inside 已兜住振荡）。坐标系全物理像素 top-left origin，"上方" = y 减小。
✅ **prewarm 竞态防线**：prewarm 隐藏创建的 webview 可能没加载完，open_preview_window reuse 路径 emit 的 preview-todo 会丢 → 后端 emit 前先存 `PENDING_PREVIEW_TODO`，preview.ts init 末尾 listeners 就位后 `take_pending_preview_todo` 主动取一次。
✅ 构建验证：`pnpm build` ✓、`cargo check --all-targets` 零警告 ✓、`cargo test --lib` 20/20 ✓。

**经验沉淀**：多窗口 hover 状态机的铁律 —— **hit test 必须把自家所有窗口视为 inside**（否则窗口 B 盖住窗口 A 时 hover emitter 振荡）；**任何 hover 驱动的显隐/反馈不准用 CSS :hover 和原生 title=**（非 key window WKWebView 都不靠谱）。

### v0.2.2（2026-08-05，预览窗即时跟手 + 消闪）

用户实测再反馈：还闪、要更快（hover 即出 / 换卡即切）、要跟手（卡左边优先、高度自适应）。根因 + 修法：

✅ **去 dwell 即时化**（main.ts 状态机重写）：去掉"截断/图片卡才预览"门控（`isTitleTruncated` 已删）—— **hover 任意卡 80ms 微防抖后立即出**（防横扫列表误触发）；**预览已开时 hover 换卡 = 0ms 立即切换**（`openPreviewFor` 直调，不走 dwell）。
✅ **跟手定位**（`preview_position` 重写）：前端传 `anchorY = card.getBoundingClientRect().top`（视口相对逻辑 px），Rust 换算 `fpos.y + anchor_y × scale`（物理屏坐标）→ **预览顶对齐被 hover 卡片顶**；左右策略：**默认卡左，左边空间不足放右边**（上/下两向砍掉），y clamp 进显示器。hover 在列表里上下移动时预览窗垂直跟手。
✅ **高度自适应一次开到位**：文本卡前端 **offscreen measurer div 预测量**（与 `.preview-text` 同宽 432px 同字体 13px/1.5 同 padding，box-sizing border-box）→ `textH` 传给 Rust → `preview_logical_size` 高 = textH + CHROME_H(66) clamp 130-720。**show 之后不再 resize**（show 后 resize 是闪的根源之一）。图片卡仍按附件宽高比。
✅ **消闪三板斧**：
  - `backdrop-refresh` 只在**隐藏→上屏**（prewarm 首显 / 全新创建）和关闭时 emit；**可见窗口换内容/跟手移动不再刷** —— v0.2.1 每次 hover 换卡都刷 → 浮窗整窗 filter repaint → 用户看到的"闪"主要来源。
  - **hover(false) 从立即关改 450ms grace close**（`schedulePreviewClose` 统一入口：unhover / hover(false) / 浮窗空白区 / preview-left 四处共用）—— 鼠标从卡片穿过缝隙进预览窗会短暂"两个窗口都不在"，立即关会把跟手体验闪断；hover(true) / over_preview / hoverCard 都会 cancel。
  - **over_preview 不再置 previewPinnedId**，改置独立的 `previewMouseInside`（仅阻止自动关闭 + 跳过 elementFromPoint）—— 路过预览窗不该把内容锁死，回浮窗 hover 别的卡仍立即切换。显式 pin 只剩两条路：点击缩略图（`openPreviewPinned`）/ 预览窗聚焦后的真实 mouseenter（`preview-entered`）。
✅ **状态机 bug 修掉**：`cancelPreviewClose()` 提到 `previewTodoId === id` 短路**之前** —— 旧代码 unhover 排的 grace close 在"快速回同一张卡"时照样炸（预览 hovering 中被关）。
✅ 构建验证：`pnpm build` ✓、`cargo check --all-targets` 零警告 ✓、`cargo test --lib` 20/20 ✓。

**经验沉淀**：① 窗口 show 前必须完成定位+尺寸（measurer 预测量 > show 后修正）；② 整窗 repaint 类修复手段（backdrop-refresh）不能挂在高频路径（hover 换卡）上，只在状态边沿（hide↔show）触发；③ 自动驻留（鼠标在某窗上）和显式 pin（用户在编辑）必须是两个状态，混用会把"路过"误判成"锁定"。

### v0.2.3（2026-08-05，预览 1Hz 闪根修 + 粘贴按钮独立框 + hint 截断修复）

✅ **预览窗 ~1Hz 闪烁根修**：元凶是 v0.2.1 加的 JS heartbeat（`setInterval` 1200ms 给 .preview-panel toggle `.force-reflow` filter 类）—— 每次 toggle 强制 backdrop 重采样，视觉上就是 ~1Hz 整窗闪（cadence 与用户实测"每秒闪一次"完全吻合）。**换成浮窗同款连续心跳动画**（preview.css `pv-backdrop-heartbeat`：rotate 0.001°/opacity 0.001 亚像素微动、compositor-only、linear 4s infinite —— 永远在动 → layer 不进 ~2s 节流窗口，且无 toggle 边沿不可见）。JS heartbeat 三处全删。**教训：backdrop 保活只能用"永远在动"的连续动画，任何周期性 class/属性 toggle 都是可见闪烁**。
✅ **粘贴按钮独立玻璃框**（用户截图布局）：`ensureInputBar` 重构 —— 新增 `.todo-input-row` flex 行容器，左 `.todo-input`（输入框+hint）右 `.todo-paste-btn`（独立瓦片，`align-self:stretch + aspect-ratio:1` 与输入框等高方形，同一套 tile 玻璃变量，`:active` 蓝色反馈）。心跳动画 / force-reflow / backdrop-refresh 三个选择器都补 `.todo-paste-btn`（它现在是带 backdrop-filter 的瓦片）。顺手删了按钮上的 `title=`（违反 v0.2.1 "不准原生 tooltip" 规则）。
✅ **hint 截断（"Press ⌘" 后半截消失）根修**：根因是 flex item 默认 `min-width:auto` —— input 不肯缩到 placeholder 内容宽以下，hint 被推出瓦片右缘。修：`.todo-input input { min-width: 0 }`（input 可缩，placeholder 框内自然截断，hint 永远完整）。`NARROW_THRESHOLD` 280 → 240（= minWidth）：完整 '⌘⇧Space' 在 240 宽也算得过来（输入瓦片 ~188px，hint 46px 稳放），tier 1 图标版只剩 <240 防御区间。
✅ 构建验证：`pnpm build` ✓（本轮无 Rust 改动）。

### v0.2.4（2026-08-05，预览黑边 + 粘贴框正方形 + hover 锁死 bug）

✅ **预览窗外圈黑边去掉**：元凶是 macOS 原生窗口阴影（builder `.shadow(true)`）—— 透明无边框窗的 NSWindow shadow 紧贴 panel 外形画一圈硬黑边。prewarm + open 两个 builder 改 `.shadow(false)`，柔和投影由 preview.css `--pv-shadow` 负责（settings 窗是 decorated 正常窗，shadow 保留）。
✅ **粘贴框正方形**：`aspect-ratio:1` 在 WKWebView 的 flex stretch 布局里 height→width 传递失效，宽度塌成 fit-content ~17px 窄条（用户截图实证）。改**固定 `width:34px`**（输入瓦片高 ≈ padding 16 + 13px 字 17px 行高 + border ≈ 34px，天然正方形）。**教训：WKWebView flex 布局别依赖 aspect-ratio 的尺寸传递，固定值最稳**。
✅ **hover 锁死 bug（鼠标进预览窗再回列表，预览不更新）**：根因链 —— 预览窗一旦拿到焦点（acceptFirstMouse 点击 / **tao `show()` 在 macOS 底层是 `makeKeyAndOrderFront:` 直接抢 key**），WKWebView 恢复派发 mouseenter → `preview-entered` 把 `previewPinnedId` 置上 → 而 `preview-left` 只在**未聚焦**时发出 → pin 永不释放 → hoverCard 的 pinned 守卫把换卡整个挡死。修法三连：
  - **pin 改焦点语义**：preview-entered 不再 pin（只 cancelPreviewClose）；新增 `preview-focused` 事件（preview.ts `onFocusChanged(focused=true)` 时 emit）才置 pin。焦点语义可靠：blur 必触发 closeSelf → preview-closed 释放，**不存在锁死路径**。
  - **show 不抢 key**：新增 `platform::show_window_no_activate`（macOS `NSWindow.orderFrontRegardless`，Win/Linux 直接 show）—— hover 路径（pinned=false）的复用 show 改走它，hover 面板永不偷焦点。
  - unlisten 清理同步补 `unlistenPreviewFocused`。
✅ 构建验证：`pnpm build` ✓、`cargo check --all-targets` 零警告 ✓、`cargo test --lib` 20/20 ✓。

✅ **输入行间隙对齐卡片节奏**（用户要求②-1）：`.todo-input-row` gap 8→6px，与 #app 卡片间 gap 一致。
✅ **直接 hover 粘贴键不变手型 bug**（②-2）：两条路径都漏了粘贴按钮 —— hover-pos 命中选择器 `.todo-copy, .todo-delete` 加 `.todo-paste-btn`（未聚焦路径），ensureInputBar 补 `mouseenter/leave → setBtnHover`（聚焦路径）。非聚焦 WKWebView 不更新 CSS cursor，set_cursor_pointer 是唯一指针通道。
✅ **预览窗被 Dock 遮挡**（②-3）：定位 clamp 用 `monitor.size()`（全屏物理分辨率）→ 改 **`monitor.work_area()`**（macOS visible frame，扣菜单栏+Dock；Win 扣任务栏）。贴底 hover 长卡时预览底边不再滑进 Dock。
✅ **预览窗 footer**（②-4）：左下 创建日期（`created_at`，epoch ms → locale 短日期）｜中 hint（flex:1 可缩省略号）｜右下 [完成日期（done 任务，取 `updated_at` —— 翻 status 时后端刷新；注意近似：done 后再改标题也会刷它）] + 复制按钮 + 垃圾桶删除按钮（二次确认实红同卡内语义，delete_todo 后 todos-changed → closeSelf 幂等双保险）。新增 i18n key `preview.created` / `preview.completed`；CHROME_H 66→74、CAPTION_AREA_H 170→178 同步。
✅ **删除按钮全 App 统一垃圾桶**（②-4）：卡内 `.todo-delete` 从 "×" 文字换 lucide trash-2 SVG（main.ts `TRASH_ICON_SVG`），预览窗删除按钮同图标（preview.ts 自带一份 13px 版）。顺手删了卡内 copy/delete 按钮残留的 `title=`（又是原生 tooltip，v0.2.1 规则的漏网之鱼）。

**经验沉淀**：④ pin/锁定类状态必须挂在**有可靠释放信号的状态**上（焦点：blur 必释放；鼠标位置：hover-pos 每 tick 上报），不能挂在"进入事件"上（离开事件可能因聚焦态/窗口层级永远发不出）；⑤ macOS 悬浮面板显示窗口永远用 `orderFrontRegardless`，`show()`/`makeKeyAndOrderFront:` 只给"用户明确要交互"的窗口；⑥ 屏幕边界 clamp 永远用 work_area 不用 monitor size（Dock/任务栏/菜单栏都在 size 里不在 work_area 里）。

### v0.2.5（2026-08-05，删除撤销 + 附件延迟删除）

✅ **删除 todo 后 8s 内可撤销**：删除仍是硬删除，但附件文件不再随 delete_todo 立即删。前端单条 undo 栈（8s 窗口）暂存被删的完整 Todo + 显示带「撤销」按钮的 action flash（`.mini-flash.has-action`，复用 `--c-data-info` 强调色）。点撤销调 `restore_todo` 恢复（图片完整可恢复）；超时调 `purge_attachment` 真删附件文件。
✅ **事件驱动统一入口**：delete_todo emit `usticky://todo-deleted`（被删完整 Todo），主浮窗 listen 后统一管 undoEntry + flash。预览窗 deleteSelf 直接调 delete_todo 也走这条路径 -> 预览窗删除也有撤销入口，不再需要各调用点各自管 undo。
✅ **附件延迟删除 + 启动孤儿扫描**：delete_todo 不删附件文件；前端 undo 栈超时调 `purge_attachment`（含路径穿越校验，禁 `/ \ ..`）真删；启动 `Store::purge_orphan_attachments` 扫 `attachments/` 目录兜底崩溃/异常退出残留。
✅ **后端**：`Store::restore`（原位塞回，防御重复 id）+ `purge_orphan_attachments`；`load_or_init` 启动调一次；新增 `restore_todo` / `purge_attachment` 命令。
✅ **前端**：`showActionFlash`（可点 toast，复用 `.mini-flash`）+ `hideMiniFlash` + 单条 undo 栈（`DELETE_UNDO_MS = 8000`）+ `undoDelete` / `clearUndoEntry`；`deleteTodo` 简化（vanishing 动画 + invoke，undo 由 listener 统一）；beforeunload 清理 `unlistenTodoDeleted`。
✅ i18n 复用现有 `app.delete.flash` / `app.action.undo` / `app.undo.flash`，无新增 key。
✅ 构建验证：`cargo check` ✓、`tsc --noEmit` 干净 ✓、`vite build` ✓。

### v0.2.6（2026-08-06，图片内联单行展示 + 输入框 Cmd+V 粘图片）

用户反馈两个问题，根因 + 修法：

✅ **① 粘贴图片后 todo 内显示空白框**：双层根因 -- a) **首启竞态**：`get_attachments_dir` 在 `init()` 里排在 `get_todos`/首次 `render()` **之后**（[main.ts](file:///Users/wyh/Project/Usticky/src/main.ts) 旧 ~1830 行），已存在附件的 todo 首屏 `buildTodoRow` 调 `attachmentUrl()` 时 `attachmentsDir` 仍 null -> `<img>` 无 `src` -> 空白边框框（且无 error 事件 -> `thumb.remove()` 不触发，框常驻）。b) **缩略图太小**：旧 `.todo-thumb` 20×20，即便加载成功也几乎看不出图，像空白框。修：a) `get_attachments_dir` 挪到 `get_todos` **之前**（[main.ts](file:///Users/wyh/Project/Usticky/src/main.ts) init）；b) 卡内 20px 缩略图换成**整宽 banner**（`.todo-banner`，`flex: 0 0 100%` + `.todo-card` 加 `flex-wrap:wrap` 让它折到标题行下方独占一行（配套把 `.todo-title` / `.todo-edit-input` 的 `flex` 从 `1 1 auto` 改 `1 1 0` -- 否则 `flex-wrap:wrap` 下长标题的 base size = 全文宽会触发换行，checkbox 独占一行、标题被挤到下一行），100px 高 `object-fit:cover` 圆角），图片真正可见，点击仍聚焦预览窗看全图 / GIF 动画。`NON_DRAGGABLE_SELECTORS` `.todo-thumb` -> `.todo-banner`。顺手删了缩略图上残留的 `title=`（违反 v0.2.1 "浮窗内不准原生 tooltip" 规则的漏网之鱼）。
✅ **② 输入框内 Cmd+V 粘不了图片**：input 是 `type=text`，默认只吃文本，粘图片静默丢弃（用户此前只能点粘贴按钮）。修：input 挂 `paste` 监听，`clipboardData.items` 含 `image/*` 就 `preventDefault` 改走 `paste_from_clipboard`（后端读 OS 剪贴板，能落附件 todo），并把已键入文字当图片 todo 的标题传进去（文字被消费进 todo 后清空 input；`kind !== "image"` 的罕见竞态不清空，保留用户文字）。纯文本放行默认插入。仅 input 聚焦时触发（窗口为 key window，paste 事件可靠）-- 未聚焦浮窗的粘贴仍由粘贴按钮兜底。
✅ **`paste_from_clipboard` 加可选 `title: Option<String>` 参数 + 纯图 todo（空标题）**（[commands/mod.rs](file:///Users/wyh/Project/Usticky/src-tauri/src/commands/mod.rs)）：图片分支标题回退链 `title` 参数 -> `img.name` -> **空字符串**（纯图 todo，前端 `.todo-title:empty` 折叠让图片占满整宽）；文本分支忽略。粘贴按钮传 None（行为不变），输入框 Cmd+V 传已键入文字。`add_with_attachment` 不校验标题（可存空）；`update_todo` 仍走 `validate_title` 拒空 -> 编辑态不能清空标题，仅 paste 能创建空标题纯图 todo。
✅ **附带：预览窗 hover 穿缝行为改进**（[main.ts](file:///Users/wyh/Project/Usticky/src/main.ts)）：预览开着时鼠标穿 card↔preview 之间的缝隙（短暂"两窗都不在"），原实现会立即摘 `.card-hover` + 落 idle 玻璃 -> 卡片像失焦闪一下。改：预览开着（`previewTodoId !== null && previewPinnedId === null`，即非 pinned 的 hover 面板）时，`floating-hover(false)` / hover-pos 不摘卡片强调、不落 idle；`over_preview` 命中时把 `rustHoveredCardId` 对齐到 `previewTodoId` 保持 `.card-hover`。释放时机统一收到 `preview-closed`：预览真关 + 鼠标仍不在浮窗（`lastHoverPayload === false`）才落 idle + 摘 `.card-hover`；鼠标在浮窗上则不动（让 hover-pos 正常驱动，避免摘了又挂抖动）。顺带删了 `previewWasOver` / `justLeftPreview` 边沿机制（被 preview-closed 释放路径取代，不再需要）。 `close_preview_window` 兜底直接 emit `usticky://preview-closed`：w.close() 在 Tauri 2 / WKWebView 下可能不触发 webview beforeunload -> preview.ts 的 beforeunload 监听不来 -> 浮窗收不到 preview-closed -> 穿缝期间保留的强调 / 玻璃卡死释放不了（只有切走 app 触发 onFocusChanged 才摘）。preview.ts beforeunload 仍会再 emit 一次，listener 幂等无副作用。
✅ 构建验证：`cargo check --all-targets` 零警告 ✓、`cargo test --lib` 20/20 ✓、`pnpm build`（tsc + vite）✓。

> **stale claim 修正（v0.2.8）**：本段 ① 描述的 `.todo-banner` 整宽 banner 方案与实际代码不符 —— 当前 [styles.css](file:///Users/wyh/Project/Usticky/src/styles.css) / [main.ts](file:///Users/wyh/Project/Usticky/src/main.ts) 是 `.todo-thumb` **内联单行 1:1** 方案（v0.2.1 形态：thumb 与 title 各 `flex:1 1 0` 占一半宽，纯图卡 `:empty` 折叠独占整宽；v0.2.1 段的描述才是现状）。历史留档不改原文，以 v0.2.1 段为准。

### v0.2.7（2026-09-04，8 域全量代码审查修复）

8 个并行审查 agent 对 8 个模块域做深度 bug 扫描（前端渲染/交互/预览/设置、后端存储/命令/平台/配置），共发现 ~95 条（按分级 P0=8/P1=22/P2=30/P3=26 报告）。本次修复 **P0（7）+ P1（8）+ P3（18）= 33 条 commit**，P2（30 条边界条件）列入 v0.3 候选。

**阶段 0**：commit 现有未提交 diff（quick-add level switching refactor，4 个文件 wip 提交）

**P0 修复（7 个 commit，必发）**：
- ✅ **`build`：rust-version 1.77 → 1.84**（u64::is_multiple_of API 需要）
- ✅ **`fix(windows)`：hover emitter hwnd 每 tick 现取 + apply_z_order IsWindow 守卫**（防 Win32 handle-reuse UB）
- ✅ **`fix(commands)`：close_preview_window focused 早期 return 也 emit preview-closed + clear pending**（grace close 状态机复位）
- ✅ **`fix(commands)`：reset_floating_window 用 work_area + hidden show/hide + 50px clamp**（多显示器拔屏位置漂移修复）
- ✅ **`fix(commands)`：update_todo no-op 返 Ok(None) 而非 Ok(Some(cur))**（语义歧义 + 双击 toggleDone 状态错乱）
- ✅ **`fix(commands)`：purge_attachment 引用检查 + remove_file 进同一 write lock 临界区**（防 delete → restore → 再 delete race 永久丢附件）
- ✅ **`fix(commands)`：open_preview_window reuse 路径 set_size/position 失败 tracing + emit 失败不返 Err**（依赖 take_pending_preview_todo 自愈）

**P1 修复（8 个 commit，高频功能性）**：
- ✅ **`fix(frontend)`：mini-flash has-action 残留 + 幽灵点击拦截层**
- ✅ **`fix(preview)`：IME 组合期间防抖保存跳过 + Esc isComposing 守卫**（zh-CN 标题永久损坏路径）
- ✅ **`fix(frontend)`：跨 section 编辑残留 + quick-add Esc 丢输入 + undoDelete 失败兜底 + hide 前清预览状态机**
- ✅ **`fix(styles)`：vanish-to-line 用 transition 两阶段替代 keyframe**（脱离全局 animation:none 禁用）
- ✅ **`fix(todo)`：损坏 JSON 链 recovered_corrupt 持久化 + 用户操作后清除**（防孤儿扫描清光附件）
- ✅ **`fix(lib)`：Moved/Resized try_write 快速路径 + CloseRequested spawn hide_dismiss + ExitRequested try_write**
- ✅ **`fix(windows/tray)`：Win hide 时 emit floating-hover false + tray 左键已走 show_normal**
- ✅ **`fix(commands)`：t! 占位符 + validate NUL + restore rollback 持久化 + shortcut 短路 + close 200ms 拆分**

**P3 修复（3 个 commit，防御性改进）**：
- ✅ **`fix(frontend)`：P3 防御性改进 — thumbWidthCache 代际校验 / deleteConfirmTimers 清 / copy 双路径失败 flash / preview 对比度 / low-power done 折叠**
- ✅ **`fix(todo/lib/tray/clipboard)`：P3 存储/线程防御 — backup 保留 5 份 + rename 降级 / persist 失败清 tmp / locale 白名单 / clipboard 注释 / tray Right Up 冗余 / FIRST_FOCUS_DONE swap 注释**
- ✅ **`chore(config)`：CSP + capabilities + CI + AGENTS.md 同步**（connect-src http://ipc.localhost + 三个纵深防御 / opener 拆 settings.json / ci permissions / docs staticlib + minWidth 240 / dragDropEnabled false）

**P2（30 条边界条件）** 列入 v0.3 候选，包括：
- thumbWidthCache 实现与注释矛盾 / 滚动 / 拖拽残留边沿 / `:empty` 在空白字符 title 下失效 / pulse 动画无 reduced-motion / 浅色背景对比度 / due 标签 DST 边界 / 多次删附件 / 预览窗 confirm 跨 todo 残留 / input Cmd+V 粘图片残留空白 / perfDump 版本硬编码等

**构建验证**：`cargo check --all-targets` 零警告 ✓、`cargo test --lib` 21/21 ✓（多一个 reorder_rejects_mixed_status_subset 回归测试）、`pnpm build`（tsc + vite）✓。

### v0.2.8（2026-10-26，置顶语义 + 预览窗粘图 + 动画卡顿根修 + 1k 卡扩展性 + 双屏位置修复）

用户反馈四项，4 个并行排查 agent 定位根因后统一修复：

✅ **① 新建/翻转置顶**（原实现全部追加到目标 section **末尾**）：
- 后端 [todo.rs](file:///Users/wyh/Project/Usticky/src-tauri/src/todo.rs)：`add_with_attachment` push 改 `insert(第一个 pending 前)` + `order = min-1`；`update` status 翻转分支同款（插到目标 section 第一行）。`position()` 找首个同 status 元素，对 pending/done 交错的 Vec 布局稳健（restore 仍 push 末尾，交错是真实状态）。order 走负数无害（无人按 order 排序，reorder 归一化回 0..N-1）。
- 前端 [main.ts](file:///Users/wyh/Project/Usticky/src/main.ts) `addTodo` 乐观 temp 同步插到第一个 pending 前（防"先底后顶"跳行）。
- 测试 21→28：改 4 个 append 断言 + 新增 7 个（add 置顶 / 附件路径置顶 / 交错 Vec / 空 pending 段 / 翻转交错 / 置顶后 reorder 归一化 / set_attachment）。
✅ **② 预览窗 Cmd+V 粘图 → 附加到当前 todo**（替换旧附件，不是新建 todo）：
- 后端新命令 `paste_image_to_todo`（spawn_blocking 读剪贴板 → 0600 落盘 → `Store::set_attachment` + persist_and_emit）；文本剪贴板 → `Err("no_image")`，空 → `Err("empty")`。旧附件文件不立即删（留启动孤儿扫描，省跨 persist 引用检查临界区）。
- 前端 [preview.ts](file:///Users/wyh/Project/Usticky/src/preview.ts) textarea paste 监听（对齐 main.ts 输入框模式），成功后拿返回 Todo 直接 `render()`（todos-changed listener 不重渲染图片）+ flashHint；img load → scheduleFit 自动按新图调窗高。新 i18n key `app.paste.no_image`；删后端死 key `commands.paste.image_title`。
✅ **③ 动画卡顿根修**（根因：**心跳动画掩蔽 transform/opacity** —— CSS animation 优先级高于 inline style，SortableJS 的 150ms 重排滑动 / 拖拽半透明 / vanish 淡出全部视觉死亡，用户看到的是"瞬移"）：
- styles.css：`.vanishing / .dragging / .sortable-chosen / #app[data-drag-active] .todo-card { animation: none }`（main.ts Sortable onStart/onEnd 挂摘 data-drag-active，兄弟卡的 inline transform 因此复活）；拖拽/塌缩本身在改几何，layer 不进 ~2s 节流窗口，玻璃防丢失不受影响。
- paint 风暴收窄：`.todo-title` transition 去掉 text-shadow（2px blur 文字阴影逐帧重栅格化 ×N 卡是 hover 边沿最贵逐帧 paint）；.todo-card/.todo-input/.todo-paste-btn transition 去掉 border-color（0→0.10 离散切换肉眼不可辨）；`.vanishing` 塌缩期间 `backdrop-filter: none` + transition 收窄到 max-height+opacity。
- **done section idle 配对 `visibility: hidden`**（0.22s 延迟到淡出完成，hover 立即恢复）：opacity:0 的不可见 done 卡此前仍带心跳 + backdrop 常驻 GPU 底噪，idle 是 99% 的状态。
- backdrop-refresh（L3）只刷视口内可见卡（rect 批量读过滤）；`text-rendering: auto`；.todo-paste-btn 补 will-change。
- JS 布局抖动：render/resize 后**一个 rAF 批量预测量**全部 thumb 宽填 cache（聚焦路径扫卡 hover 边沿零 offsetWidth 强制布局）。
✅ **④ 1k 卡扩展性**（渲染已是 keyed diff，本轮补超线性/浪费点）：
- `refreshDueLabels` O(N²)→O(N)（Map 建表；N=10k 每 30s 从 ~100M 次比较降到 ~10k）。
- `updateTodoRow` title/due/aria 同值守卫（同值 textContent 赋值仍替换文本节点）。
- **hidden 渲染门**：浮窗 hidden 期间 todos-changed 跳过 render（latestSnap 暂存），visibilitychange→visible 补一次。
- `diffSection` 单值→多值 Map（Musage 2026-09-07 同款重复 key 防御）。
- `check.title` 原生 tooltip（浮窗内最后残留）→ aria-label。

✅ **⑤ 双屏重启后浮窗骑在两屏交界处**（用户实测：macOS 双屏并排，重启后窗口落在两块屏幕中间）：
- 根因：[lib.rs](file:///Users/wyh/Project/Usticky/src-tauri/src/lib.rs) setup 恢复位置时把保存的 (x, y) **clamp 到主显示器边界** —— 本意是"副屏拔了别把窗口扔屏外"，但它不认识副屏：窗口上次合法地在副屏（坐标超出主屏范围），重启后被强行拉到主屏右缘，恰好 = 双屏交界处；且用的是 `monitor.size()` 而非 work_area。
- 修（一版）：新增 `clamp_restore_pos` 多屏感知纯函数（`MonitorRect` 快照脱离 AppHandle 可单测）—— 坐标仍落在某块屏**全屏矩形**内 → 恢复到那块屏；所有屏都不含该点（屏被拔/排列变了）→ 选**最近屏**（点 clamp 进各屏矩形算距离平方取最小）；拿不到显示器列表 → 原坐标不动。setup 恢复接入 `collect_monitor_rects(app.handle())`。
- **修（二版，竖条回归）**：一版只保证**左上角** 60px 可见 —— 保存坐标走"最近屏"分支时，300px 宽的窗口被钉在"目标屏右缘 - 60"，其余 240px 悬在屏外无显示器的空间，用户看到贴屏幕右缘的 ~35px 竖条（那列圆圈是各卡勾选圈、竖排文字是压到一字宽的 section 标题；`resize_floating_window` 只改高，窗口宽度从未塌，纯粹是位置问题）。二版升级为**整窗收纳**：clamp 带 win_w/win_h（setup 从持久化 geom 取，缺省 outer_size），窗口放得下时 x ∈ [work 左, work 右 - win_w]（整窗在屏内）；放不下（窗口比屏宽/高）才退化为 min-visible。整窗收纳同时治好"命中 A 屏但窗口伸进 B 屏（骑缝）"的恢复场景。
- 测试 28→37：副屏原样恢复（核心回归）/ 主屏不误伤 / work_area 顶 clamp / **副屏拔掉整窗收纳**（竖条回归）/ **骑缝拉回整窗** / 高于屏的窗口顶部可见 / 左侧负坐标副屏 / 空列表返 None / 小屏 clamp 区间退化不 panic。

**100/1k/10k 扩展性结论**（详见排查报告，本轮已实装 1k 档）：
- ≤500：现有架构（keyed diff + 3 委托监听 + rAF 合并）完全够。
- ~1k：本轮四项后每次 CRUD 成本毫秒级；payload ~250KB 可接受。
- ~10k 需要（v0.3 候选）：`.todo-card { content-visibility: auto; contain-intrinsic-size: auto 44px }`（屏外卡跳过 layout/paint，~3 行 CSS，需实测与 backdrop/SortableJS 交互）；todos-changed 改增量事件（`{rev, upserted, removed, order}`，同时解决 emit MB 级 payload 和 snapshot 双深拷贝）；reorder 锚点增量协议；**不建议虚拟化**（scrollHeight 自适应 + SortableJS drop index + reorder 完整排列校验三重依赖全量 DOM，c-v:auto 已等价"渲染虚拟化"）；**存储继续 JSON 不换 SQLite**（单文件原子写 + epoch + 5 份 bak 的防线优于引入 rusqlite 的复杂度，换库也不解决 IPC payload）。

**构建验证**：`cargo check --all-targets` 零警告 ✓（clippy 警告 28→27，未引入新警告）、`cargo test --lib` 28/28 ✓、`pnpm build`（tsc + vite）✓。

**经验沉淀**：⑦ CSS animation 对其声明属性的优先级高于 inline style 且锁死 computed value —— 任何"用 inline transform 做动画"的库（SortableJS animate / FLIP）与常驻 keyframes 动画（心跳保活）在同一元素上互斥，交互态必须暂停后者；⑧ `visibility: hidden` 是 opacity:0 的免费配套（跳过绘制/合成/命中测试），配对延迟 transition 可保 0 跳位；⑨ 列表项常驻保活动画（心跳）+ backdrop-filter 是按 N 线性增长的 GPU 底噪，不可见子树要显式退出合成。

### v0.2.9（2026-10-27，多行内容换行丢失修复）

用户反馈 todo 内容有时丢换行。根因：HTML `<input type=text>` 的 **value sanitization 是规范行为 —— 会剥掉 value 里全部 `\n`/`\r`**（赋值 / 粘贴 / 拖入都逃不掉），而项目有两条路径用 text input 承载可能含换行的标题（后端 `validate_title` 注释明确"换行 trim 后保留"，换行本是被支持的设计）：

✅ **① 浮窗编辑态 `<input>` → `<textarea rows=1>`**（静默数据丢失主因）：多行 todo（粘贴按钮 / 预览窗创建）一点进浮窗编辑，`input.value = todo.title` 瞬间剥掉换行；此后 commit 里 `next !== todo.title` 恒真 —— **用户什么都没改，blur/Enter 提交也把剥掉换行的版本 update_todo 写回磁盘**。修：编辑控件改 `textarea.todo-edit-input`（value 保留 `\n`；rows=1 + 同 font/padding/border + `margin:0; resize:none; overflow:hidden; white-space:pre` 与旧 input 等高 0 抖动）；**Enter 提交 / Shift+Enter 插入换行**（textarea 内部滚动，卡片保持单行紧凑，全文看预览窗）/ Esc 取消；顺手补 IME 守卫（`isComposing || keyCode 229` 不触发 commit，防 zh-CN 选词 Enter 提前提交半截组合文本，对齐 preview.ts v0.2.7 P1）。配套改 4 处 `input.todo-edit-input` 选择器 → `textarea.todo-edit-input`（**exitEditMode 那处漏改 = cleanup 永远找不到元素**，isEditing 守卫永不释放）。
✅ **② quick-add 输入框粘贴多行文本**（创建路径丢失）：WebKit 往 text input 默认粘贴插入剥 `\n` 成空格。paste listener 补文本分支：`getData("text/plain")` 含 `\n` → preventDefault + 已键入文字作第一行 + 粘贴文本（`\r\n` 归一 `\n`）拼接走 `addTodo`（后端 validate_title 只 trim 首尾，内部换行原样落盘）；单行放行默认插入。失败回填只回填已键入文字（input 存不了 `\n`），粘贴部分仍在剪贴板可重试；`pastePending` 防与 Enter 双建（同图片分支）。
✅ **显示层维持 `white-space: nowrap`**：卡片单行紧凑是设计 —— `textContent` 渲染数据无损（`\n` 视觉折叠为空格 + 省略号），多行全文看预览窗（`.preview-text` textarea + pre-wrap）。未改。

**经验沉淀**：⑩ `<input type=text>` 的 value 带"无换行不变量"（HTML 规范 value sanitization）—— 任何可能含多行文本的字段，录入和编辑都必须走 `<textarea>` 或后端路径，且"改了控件类型"必须全量 grep 标签选择器（`input.xxx` → `textarea.xxx`，漏一处该路径静默找不到元素）。

**构建验证**：`pnpm build`（tsc + vite）✓（本轮无 Rust 改动）。

### v0.2.10（2026-11-03，玻璃心跳从「每卡一个」降为「窗口一个」哨兵）⚠️ **未合入 main，在分支 `perf/window-keepalive-glass`（commit 5330edf）**

> **状态**：分支已推 origin，代码完整、`pnpm build` 通过，但**用户尚未做视觉验收**（本环境无法截图）。下一个 agent 的第一件事是跑 `pnpm tauri dev` 按下面的验收清单验证。
> 被否决的旧方案留在 `perf/single-glass-panel`（commit b334114，已推 origin）—— **不要合它**，理由见下。

**触发**：用户反馈"总是有卡顿感"，并提过"有个新 todo 的想法"（**至今未说明**）。诊断后先试了"整窗单面板"（`perf/single-glass-panel`），用户实测：**动效确实丝滑多了，没有卡顿感**，但暴露三个视觉回归 —— ①未 hover 时底部一大片背景模糊留白 ②hover 后玻璃覆盖不足（底边 95px 无玻璃 + 固定面板不跟随滚动内容）③**卡间透明缝隙消失**（原来"圆角胶囊 + 透明间隙"的观感没了）。用户明确问"能改回来吗"。

**根因澄清（关键，别被"单面板"这个表象带跑）**：卡顿感其实是**两个独立原因**，整窗单面板只是把两个都顺手绕过去了：

1. **GPU 风暴（O(N)）**：旧实现每张卡各挂一份心跳动画（`transform`/`opacity` 0.001° 亚像素微动）。N 个持续动画的合成层 → **每帧 N 次 backdrop 重采样**。卡一多就掉帧。= v0.2.8 经验 ⑨。
2. **掩蔽交互动画（这是"瞬移感"的真凶）**：v0.2.8 经验 ⑦ —— **运行中的 CSS `animation` 对它声明属性的优先级高于一切普通声明，包括 inline style，且动画期间该属性的 computed value 被锁死**。心跳 keyframes 声明了 `transform` → SortableJS 归位时写的 **inline `transform`**（150ms 重排滑动）被完全掩蔽 → **松手后所有卡瞬移到位**。v0.2.8 用"交互态暂停心跳"打了补丁，但那只覆盖拖拽**进行中**，松手后的**归位滑动**仍被掩蔽。

⚠️ **推论：用户感受到的"丝滑"主要来自原因 2，不是来自"采样层数从 N 变 1"。** 所以正解不是"砍成一块玻璃"，而是"**砍掉卡片上的动画**"。

**最终架构（v0.2.10）**：**逐卡玻璃 + 窗口级心跳哨兵**

| 层 | 位置 | 机制 | v0.2.10 变化 |
|---|---|---|---|
| L1 | `styles.css` `.todo-card/.todo-input/.todo-paste-btn` | `will-change: backdrop-filter` → 独立 GPU layer，不进层级空闲节流 | **不变**（逐卡保留）|
| L2 | `styles.css` `.glass-keepalive` + `index.html` | **窗口级** 1px 哨兵，持续微动 → 合成器永不空闲 → 全部瓦片的 backdrop 采样一起被保住 | **改**：从"每卡一个"降为"窗口一个" |
| L3 | `macos.rs` `set_window_level` → `usticky://backdrop-refresh` | level 切换后 force reflow 立即重采 | **不变**（仍只刷视口内可见卡）|

- **心跳不再挂在卡片上** → 卡片上**零 `animation`** → 没有任何属性被锁死 → SortableJS 的 inline transform 归位滑动、`.dragging`/`.sortable-chosen` 的 opacity 淡化、`.vanishing` 的淡出**全部原生生效**。v0.2.8 那套"交互态暂停心跳"补丁连同 `main.ts` 的 `data-drag-active`（3 处）**整套退役**。
- **成本 O(N) → O(1)**：每帧只重采样 1 个 1px² 层，而不是 N 个卡片层。
- **哨兵节点**：`<body>` 下 `<div class="glass-keepalive">`（`index.html`），`position: fixed` → 相对浮窗视口，不受 `#app` 滚动 / `render()` 的 DOM 增删影响。**刻意不用 `var(--tile-blur)`** —— 瓦片变量定义在 `#app` 规则上，哨兵是 `#app` 的兄弟节点继承不到，`var()` 无效会让 `backdrop-filter` 计算成 `none`（等于没加）。哨兵自己带 `backdrop-filter: blur(10px) saturate(180%)`，让它是**真正的 backdrop-filter 元素且持续被 invalidation** —— 走的正是旧版"每卡心跳"完全相同的机制，只是数量 1 个。

**为什么保留逐卡玻璃（否决整窗单面板）**：单块连续玻璃在物理上**切不出卡间的透明缝隙** —— 模糊必须逐瓦片才能挖出缝，这是"圆角胶囊 + 透明间隙"观感的唯一来源。同理，单面板会盖住 done 段的保留区（`visibility:hidden` 但仍占布局高度）→ 就是用户看到的"大面积模糊留白"。

**改动文件**：`index.html`（+9 哨兵节点）、`src/styles.css`（心跳搬家 + 删"交互态暂停心跳"整段 + 3 处 stale 注释）、`src/main.ts`（删 `data-drag-active` ×3）。

**验收清单（下一个 agent / 用户跑）**：

> 📄 **可复制的 console 诊断套件**：[docs/diag/glass-ab-test.md](file:///Users/wyh/Project/Usticky/docs/diag/glass-ab-test.md)（898 行，含 17 段可直接粘贴的 console 片段、WebKit 坑清单、结果记录表）。下面第 3 条的核心 A/B 实验在该文档 §4 有完整脚本 + 阳性对照 `E3` + 失败分支排查。
> ⚠️ **实验前必须做的三件事**（否则结论无效）：
> ① **切到 PinTop**（设置面板）—— 默认档是 `PinBottom`（`PinMode::default()`），其 hover-raise 会切 NSWindow level → emit `usticky://backdrop-refresh` → L3 给可见卡加 `.force-reflow` **强制重采样** → 会把"玻璃掉了"洗成假阴性。
> ② **devtools Undock 成独立窗口**并摆在浮窗旁边 —— Tauri devtools 默认停靠应用内，鼠标移进 console 浮窗就失焦 → `body[data-hover]` 被清 → 玻璃强弱随鼠标进出跳变，读不出结论。
> ③ 浮窗别被别的窗口完全遮住 —— WKWebView 会暂停合成/节流 rAF，探针采不到样本。

1. `pnpm tauri dev` → 未 hover：卡片是**独立胶囊**，**卡间是透明缝**（能看清桌面），**底部 done 保留区不铺玻璃**（不该有大片模糊）。
2. hover：done 段显形，每张卡各自带玻璃，**卡间仍留缝**，滚动到底**每张卡都还有玻璃**（不该有未玻璃的底边带）。
3. **静置 10s 玻璃不消失**（L2 哨兵是否真能保住其它卡片的采样 —— 这是本方案唯一未经验证的关键假设：节流是**窗口级**判定，若实际是元素级，玻璃仍会在 ~2s 后掉）。
   - **计时协议**：移除哨兵这个 DOM 变更本身会触发一次 invalidation → 玻璃在移除瞬间必被重采样一次。**计时起点是"鼠标移回浮窗之后"**，等 ≥6s 再判读。
   - **阳性对照 `E3`**：给所有瓦片加 `.force-reflow`（CSS 里 blur 10px→11px，强制 WebKit 重采样）。**玻璃掉了 + E3 按下立刻回来** → 掉的确实是 backdrop 采样 → 假设成立；**掉了 + E3 按下不回来** → 不是采样被丢弃（瓦片丢了滤镜/被盖住/低电量）→ 另找原因。
4. 拖拽排序：松手后卡片是**滑动**归位，不是瞬移（验证原因 2 已修）。
5. 标记完成：卡片淡出 + 塌缩，**有淡出**不是直接消失（v0.2.8 被掩蔽的老问题）。
6. 低电量模式：玻璃整体关闭（`body[data-low-power] *` 的 `animation:none` + `backdrop-filter:none` 覆盖哨兵，此时无玻璃可保，无副作用）。
   - ⚠️ **注**：`body[data-low-power]` 属性**当前前端并未接线**（`src/*.ts` 零命中）→ 这条规则今天不会触发，哨兵的 blanket-kill 是理论隐患而非现存 bug。

**若第 3 条失败（玻璃 2s 后掉）**：说明节流是元素级而非窗口级。**先跑 `E3` 阳性对照确认掉的确实是 backdrop 采样**（见验收清单第 3 条），再按代价排序选备选：

- ①**把哨兵放大成覆盖整个视口的隐形 backdrop 面板**（`position:fixed; inset:0; opacity:0.001`）—— 仍是 O(1) 个合成层，但每帧重采样面积从 1px² 变整个视口。这也是"窗口级假设"的最强形式：连它都保不住采样，说明方向全错。
- ②**恢复逐卡心跳，但只动画 `opacity` 不动画 `transform`** —— transform 不再被锁 → SortableJS 归位滑动正常；opacity 被锁 → `.dragging`/`.vanishing` 淡出失效，**补救办法是把淡入淡出搬到 `.todo-card::before`**（伪元素从不挂 animation，且 `styles.css` 早就把玻璃背景搬到 `::before`、它已带 `transition: opacity 0.22s`）。代价：退回 O(N) 动画层，GPU 风暴回来。
- ③`perf/single-glass-panel`（commit b334114）—— **已被用户否决**，选它之前必须先跟用户确认（3 个视觉回归：底部大片模糊留白 / 底边 95px 无玻璃 + 面板不跟随滚动 / 卡间透明缝消失）。

**⚠️ 反向分支（更常见，优先考虑）**：如果 A/B 实验里**移除哨兵后玻璃压根就不掉**（不是"掉了又回来"）—— 那说明当前配置下根本没触发 WKWebView 的空闲节流（可能是 macOS 版本 / 窗口层级 / 是否 key 的差异），**这时最优解不是上备选 ①，而是把 L2 心跳整层删掉**（连哨兵一起删）—— **零 GPU 成本**，且卡片上零 animation 的核心收益（经验 ⑦ 的治本）完整保留。排查用的 console 片段见 [docs/diag/glass-ab-test.md](file:///Users/wyh/Project/Usticky/docs/diag/glass-ab-test.md) §4.5（B-1a/b：停掉心跳但保留元素 → 区分"是动画在起作用"还是"元素存在就够了"；L1 `will-change` 是否已足够；是否其实是被遮挡导致的另一个 bug）。

**后续候选**（未做）：原生 `windowEffects`（macOS `NSVisualEffectView` / Win11 Acrylic）可把整套 CSS 玻璃 workaround 删掉，但**同样是整窗矩形模糊 → 同样切不出卡间缝**，与"逐卡玻璃"目标冲突，需先决定是否放弃缝隙观感；`prefers-reduced-motion` 支持（v0.2.7 P2 遗留）。

**经验沉淀**：⑪ **"降采样成本"和"降动画数量"是两件事，别用前者换后者** —— 单面板方案省下了采样层数，代价是丢了逐瓦片才有的视觉特性；而卡顿感的另一半（animation 掩蔽 inline style）跟采样层数毫无关系。⑫ **CSS animation 掩蔽 inline style 的连带效应**：任何"保活"用的常驻动画，如果声明了 transform/opacity，就会连带禁掉该元素上一切依赖 inline style 的第三方动画（SortableJS 的 animate() 就是典型）。保活动画应该挂在一个**专用的、不参与任何交互**的哨兵元素上。⑬ **backdrop-filter 的节流是窗口级还是元素级，决定了保活方案能不能"只做一个"** —— 本项目的判断是窗口级（依据：玻璃是**整窗同时**丢失的、backing store 是窗口级资源），但**尚未实测验证**。⑭ **验证"玻璃会不会掉"之前，先排除掉所有会主动重采样的路径** —— 否则 L3（`backdrop-refresh` → `.force-reflow`）会把失败洗成假阴性（PinBottom 默认档的 hover-raise 就走这条路）；同理 devtools 停靠会偷走焦点打断 hover 状态、被遮挡窗口会被节流合成。**做 A/B 实验必须：切 PinTop + devtools Undock + 窗口不被遮挡 + 计时从"鼠标移回"开始 + 带阳性对照**。

**构建验证**：`pnpm build`（tsc + vite）✓（本轮无 Rust 改动）。

### 仍未做

⏳ **Cmd+Z 撤销栈**（[main.ts](file:///Users/wyh/Project/Usticky/src/main.ts) 已占位 keydown listener，TODO 未实现，v0.2 候选）
⏳ **tray 图标任务数 badge**（v0.1 是静态图标 `tray-base.png`，v0.2 候选）
⏳ **用户提过的"新 todo 想法"**（2026-11-03 起说了两次，**始终未说明内容**）—— 下次和用户对话时**先问清楚**再动手，别自行猜测实现
⏳ **v0.2.10 玻璃方案视觉验收**（分支 `perf/window-keepalive-glass`）—— 见 v0.2.10 段验收清单 6 条；未验收前不合 main

## 发版（v0.2.0 起，2026-07-21）

沿用 Musage 流水线打法（`.github/workflows/`）：

- **CI**（`ci.yml`）：PR + main push 触发。frontend（pnpm build 单平台）+ rust（三平台 `cargo check --locked --all-targets`，Linux 需 webkit2gtk-4.1 系统依赖）。
- **Release**（`release.yml`）：push `v*.*.*` tag 或 Actions 手动输入版本号触发。矩阵 = macOS arm64 dmg / macOS x64 dmg / Windows x64 NSIS（MSVC）/ Linux x64 AppImage+deb（pin ubuntu-22.04），`tauri-action@v1.0.0`（钉 SHA）出 **draft release** + verify job 校验 5 个产物齐全。
- **产物未签名**：没配 APPLE_* / WINDOWS_CERTIFICATE secret。macOS 用户右键打开或 `xattr -cr`；Windows SmartScreen 选"仍要运行"。以后要签名时把 Musage release.yml 的 env 块抄回来。
- **有意砍掉**：Windows MSI（Musage 实测 WiX 镜像 CI 上常 timeout）、Linux RPM（AppImage+deb 覆盖面已够）。
- **版本号三处同步**：`pnpm sync-version`（package.json → tauri.conf.json + Cargo.toml）。发版流程：改 package.json version → sync → 更新 CHANGELOG → commit → `git tag vX.Y.Z && git push origin vX.Y.Z` → 等 draft release → 人工 review 后 publish。
- Windows 目标用 **MSVC**（Musage CI 同款）。Cargo.toml 的 `crate-type = ["rlib"]`（v0.1.5 P3-4 删 staticlib 后）。
- 不上传 `latest.json`（不走 tauri-plugin-updater；要加 updater 时记得把 network entitlement 加回 entitlements.plist）。

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
├── preview.html              ← QuickLook 预览窗入口（动态创建 webview，v0.2）
├── src/
│   ├── main.ts               ← 浮窗：渲染 + 拖拽 + 输入 + 快捷键 + hover 双路径 + 粘贴按钮 + 预览状态机
│   ├── styles.css            ← iOS 26 玻璃质感（沿用 Musage）
│   ├── preview.ts            ← 预览窗：图片/长文展示 + textarea 自动保存编辑（v0.2）
│   ├── preview.css           ← 预览窗样式（v0.2）
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
        ├── todo.rs           ← Todo + TodoAttachment + PinMode + StoreData + JSON storage（原子写 + 0600 + .bak）
        ├── clipboard.rs      ← 剪贴板读取三路径（macOS NSPasteboard 原字节保 GIF / 插件 RGBA 兜底 / 文本，v0.2）
        ├── tray.rs           ← 系统托盘（Settings 子菜单 + pin mode checkmark + rebuild_tray）
        ├── commands/
        │   └── mod.rs        ← CRUD + 浮窗控制 + i18n + pin mode + open_settings_window + 剪贴板粘贴/预览窗（v0.2）
        └── platform/
            ├── mod.rs        ← 跨平台统一 API（pub use plat::*）
            ├── macos.rs      ← PinBottom/PinTop/Normal + hover emitter（已实现）
            ├── windows.rs    ← HWND_TOPMOST/BOTTOM dual-path + hover emitter（best-effort）
            └── (linux)       ← mod.rs 内 no-op stub（无 linux.rs 文件）
```