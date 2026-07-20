# Changelog

所有值得记录的变更都会写到这里。格式基于 [Keep a Changelog](https://keepachangelog.com/)。

## [Unreleased]

### Added

### Changed

- **hover 背景与 Musage 逐项对齐，两 app 同屏颜色统一**：hover
  `--tile-bg` 0.92 → 0.82、`--tile-border` 0.12 → 0.10、`--tile-shadow`
  0.50/0.05 → 0.45/0.04（此前 Usticky hover 比 Musage 深一档，同屏时
  玻璃色不一致）。idle→hover 差随之与 Musage 相同（0.30 → 0.82 =
  0.52 alpha）。blur 28px / saturate 180% 两边本就一致，不动。
- **idle（未 hover）背景压到 Musage 档位，hover 才显现**：`--tile-bg`
  alpha 0.55 → 0.30、`--tile-border` 0.06 → 0、`--tile-shadow` → 0
  （对齐 Musage idle：0.30 alpha / border 0 / shadow 0），idle 几乎只剩
  文字浮在桌面上，hover 才显出玻璃瓦片（idle→hover 差 0.62 alpha）。
  done 卡 idle 同步 0.24 → 0.12 保持"更折叠"相对关系。blur 仍 28px
  写死不跟 Musage 的 10px —— 那是 WKWebView backdrop throttling
  三层防御的前提。低电量模式（锁全强度）不受影响。
- **拖拽时 checkbox 变 ⇅ 上下指示符**：旧版拖拽中 `.todo-check` 被
  `display:none` + `padding-left:11px`，文字左挤。现在 checkbox 圆圈
  在 dragging / sortable-chosen 态变成 ⇅（占住原槽位，文字不位移），
  同时明示"正在拖的是这张"；delete / due 标签拖拽中仍隐藏。
  done 卡拖拽同样显示 ⇅（`#app` 前缀压过 done 的绿底 ✓）。
- **清理网络 entitlement 对齐"不联网"产品承诺**：删除
  `entitlements.plist` 的 `com.apple.security.network.client`。
  Usticky v0.1 实际零网络请求（前端静态 import + CSP 禁非 self/ipc
  connect），保留 entitlement 只会扩大 Hardened Runtime 攻击面。
  v0.2 加 Tauri updater 时再加回来。AGENTS.md / README 的"不联网"
  承诺现在跟二进制实际权限一致。

### Fixed

- **拖拽 todo 卡时浮动克隆遮挡内容**：SortableJS
  `forceFallback + fallbackOnBody` 会把被拖卡克隆一份 append 到
  `document.body`，以 `position:fixed + z-index:100000` 跟随光标；
  且克隆脱离 `#app` 的 CSS 变量作用域，卡片外观变量全部失效，
  退化成透明底裸文字盖在最顶层。落点本已由列表内 `.dragging`
  占位卡实时演算展示，浮动克隆纯属冗余 —— 直接
  `.todo-card.sortable-drag / .sortable-fallback { visibility: hidden }`
  整体隐藏（visibility 无内联样式冲突，克隆保留盒模型，不影响
  SortableJS 内部 transform 更新 / drop 移除逻辑）。
- reset_floating_window 加 `available_monitors().first()` fallback
  （Wayland 等场景下 `primary_monitor()` 返 None）+ tracing 日志
  输出目标显示器。

#### v0.1.0 骨架（2026-07-02）

- Tauri 2 项目结构 + 双 locale i18n（en + zh-CN）+ iOS 26 玻璃质感 CSS + 浮窗位置/尺寸自动记忆
- IPC 接口：`list` / `add` / `update` / `delete` / `reorder` + `get_app_locale` / `set_app_locale`
- 全局快捷键 `CmdOrCtrl+Shift+Space` → `usticky://quick-add` → 聚焦 input
- JSON 持久化：原子写（tmp → rename）+ Unix 0600 + 解析失败 backup `.bak.<ts>`
- `WindowEvent::Moved` / `Resized` → spawn 异步任务持久化（不阻塞 UI 线程）
- 关闭 = 隐藏（`api.prevent_close()` + `window.hide()`），tray 左键单击切换显隐

#### v0.1.1（2026-07-02，搬 Musage 三档 pin mode）

- `PinMode` enum（PinTop / PinBottom / Normal）+ 持久化到 `todos.json`
- macOS：`NSWindow.setLevel` 切三档（`kCGFloatingWindowLevel` / `kCGNormalWindowLevel - 1` / `kCGNormalWindowLevel`）
- Windows：`HWND_TOPMOST` / `HWND_BOTTOM` / `HWND_NOTOPMOST` dual-path（`SetWindowPos` + `SetWindowLongPtrW` 改 `WS_EX_TOPMOST` style bit）
- Linux：no-op stub（`set_always_on_top(true)` 已是最实用方案）
- `get_pin_mode` / `set_pin_mode` 命令 + `usticky://pin-mode-changed` 事件

#### v0.1.2（2026-07-03 → 2026-07-06，hover emitter + 设置面板 + tray 子菜单）

- **Hover emitter**：50ms tick 全局鼠标轮询
  - macOS：`NSEvent.mouseLocation` + `NSWindow.windowNumberAtPoint` 命中测试（不仅检查鼠标在 frame 内，还确认浮窗是该点 topmost）
  - Windows：`GetCursorPos` + `WindowFromPoint` + `GetAncestor(GA_ROOT)` 命中测试
  - Dwell-time hysteresis（enter 3 ticks / exit 2 ticks）防边缘抖动振荡
  - 永远 emit `usticky://floating-hover`（驱动 CSS `body[data-hover]` 玻璃效果，不分 pin mode）
  - PinBottom 模式额外切 NSWindow level / Win z-order（hover 临时置顶）
- **Hover 双路径**：Rust emit + JS `mouseenter`/`mouseleave` 40ms debounce；失焦时主动 `setHoverAttr(false)` 清 stale state；visibilitychange 清理
- **SortableJS 拖拽排序**：pending / done section 各一个 Sortable 实例，`onEnd` 批量 `reorder_todos`
- **标记完成动画**：`.vanishing` class + 300ms 延迟后才调 IPC，失败回滚 class
- **设置面板**（`settings.html` + `src/settings.ts` + `src/settings.css`）：单页设计
  - 浮窗层级（pin mode）segmented control
  - 语言切换（en / zh-CN）
  - 浮窗归位到主屏幕正中央（`reset_floating_window`）
  - 关于（版本 / 产品名 / GitHub）
- `open_settings_window` 命令：动态创建 webview（已开则 focus，关闭时 destroy，不在 tauri.conf.json 常驻）
- **Tray Settings 子菜单**：pin mode 三档 `CheckMenuItem`（带 checkmark）+ "Open Settings Panel..."
  - locale / pin mode 切换时 `rebuild_tray` 走 `run_on_main_thread` 派发避免 NSStatusBar `assertBarrierOnQueue` SIGTRAP
- **Tray icon 改 U 字母**（`scripts/generate_icons.py`）：白底圆角 + 黑色加粗 U + ring 装饰；每个尺寸原生渲染（不降采样）；macOS 用 `iconutil` 拼真 `.icns`
- 浮窗控制命令：`reset_floating_window` / `resize_floating_window` / `hide_floating_window` / `show_floating_window`
- `set_floating_hover_raise` 命令（前端兜底信号，macOS/Win 上 tracker 已自行处理，此处 no-op）
- **locale 切换链路**：tray + settings 窗口 title 同步重建（`usticky://locale-changed` listener）
- **Pin mode 跨 webview 同步**：`usticky://pin-mode-changed` listener 在浮窗 / 设置面板 / tray 三处生效
- `persist_and_emit` 失败时 emit `usticky://persist-failed`（不再静默吞掉，前端 mini-flash 提示）
- 启动时恢复浮窗位置 clamp 到主显示器范围内（防副屏拔了之后窗口扔到屏幕外）

### Fixed（v0.1.2 调试期间）

- PinBottom 模式 hover 误置顶 + 毛玻璃效果振荡消失（dwell-time hysteresis 阈值改回 Musage 的 3/2）
- hover 玻璃效果在 transparent 区域消失（改回 `windowNumberAtPoint` 命中测试，不用 `frame.contains`）
- hover 玻璃效果在启动时丢 —— 必须点一下才生效（去掉 `!focused` 守卫，un-focused 浮窗的合法 hover 不该被吞）
- hover 玻璃效果在切回浮窗时丢（`setHoverAttr(false)` 重置 dedup 状态）
- 光标移上浮窗时闪烁（hover emitter 同值去重 + JS 40ms debounce）
- 浮窗位置 `set_position` 在副屏拔了之后扔到屏幕外（启动时 clamp 到主显示器范围）

### Pending（next sprint）

- Cmd+Z 撤销栈（最多 50 条）—— `main.ts` 已占位 keydown listener，TODO 未实现
- 全局快捷键冲突检测
- 全文搜索（Cmd+F 浮窗内）
- tray 图标任务总数 badge（v0.1 是静态图标 `tray-base.png`）
- 提醒通知（tauri-plugin-notification，临近 deadline 弹）
- 标签分组（折叠）

## [0.1.0] - 2026-07-02

### Added

- 项目初始化（forked from Musage v0.2.0 浮窗经验）
