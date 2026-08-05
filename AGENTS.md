# Usticky 项目说明

> 任何新打开此项目的 AI 会话应先读这个文件。当前快照：**v0.2.4 剪贴板粘贴 + QuickLook 预览（消闪收尾 + pin 焦点语义 + 预览 footer 日期/按钮）** / 2026-08-05。

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

### 3. 渲染：当前为全量重建（v0.1.5 仍如此），未来迁移到增量 diff

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
✅ **Todo.attachment 字段**（`TodoAttachment { file, mime, width, height }`，serde default 向后兼容）：只存相对文件名，绝对路径运行时拼（`Store::attachments_dir()`）。delete_todo 连带删附件文件（NotFound 不算错）。
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

✅ **① hover 显示慢**：dwell 600/350ms 太长 + 首次创建 webview 300-500ms 白屏。修：dwell 砍到 **文本 350ms / 图片 250ms**（main.ts `PREVIEW_DWELL_TEXT/IMAGE_MS`）；首次 floating-hover(true) 时 `prewarm_preview_window` **隐藏创建**预览窗（webview 提前加载完），后续 dwell open 只剩定位+show。
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

### 仍未做

⏳ **Cmd+Z 撤销栈**（[main.ts](file:///Users/wyh/Project/Usticky/src/main.ts) 已占位 keydown listener，TODO 未实现，v0.2 候选）
⏳ **tray 图标任务数 badge**（v0.1 是静态图标 `tray-base.png`，v0.2 候选）

## 发版（v0.2.0 起，2026-07-21）

沿用 Musage 流水线打法（`.github/workflows/`）：

- **CI**（`ci.yml`）：PR + main push 触发。frontend（pnpm build 单平台）+ rust（三平台 `cargo check --locked --all-targets`，Linux 需 webkit2gtk-4.1 系统依赖）。
- **Release**（`release.yml`）：push `v*.*.*` tag 或 Actions 手动输入版本号触发。矩阵 = macOS arm64 dmg / macOS x64 dmg / Windows x64 NSIS（MSVC）/ Linux x64 AppImage+deb（pin ubuntu-22.04），`tauri-action@v1.0.0`（钉 SHA）出 **draft release** + verify job 校验 5 个产物齐全。
- **产物未签名**：没配 APPLE_* / WINDOWS_CERTIFICATE secret。macOS 用户右键打开或 `xattr -cr`；Windows SmartScreen 选"仍要运行"。以后要签名时把 Musage release.yml 的 env 块抄回来。
- **有意砍掉**：Windows MSI（Musage 实测 WiX 镜像 CI 上常 timeout）、Linux RPM（AppImage+deb 覆盖面已够）。
- **版本号三处同步**：`pnpm sync-version`（package.json → tauri.conf.json + Cargo.toml）。发版流程：改 package.json version → sync → 更新 CHANGELOG → commit → `git tag vX.Y.Z && git push origin vX.Y.Z` → 等 draft release → 人工 review 后 publish。
- Windows 目标用 **MSVC**（Musage CI 同款）。Cargo.toml 的 `crate-type = ["staticlib", "rlib"]` 是给 MinGW 兜底的，对 MSVC 无副作用，别删。
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