# Changelog

所有值得记录的变更都会写到这里。格式基于 [Keep a Changelog](https://keepachangelog.com/)。

## [Unreleased]

### Added

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
