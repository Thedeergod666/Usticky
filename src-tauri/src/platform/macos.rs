//! macOS 特定：两件事
//!
//!   1. 把浮窗的 NSWindow level 直接设到非 0 位置，实现"始终置底/置顶"。
//!   2. **全局鼠标位置轮询，把 hover 状态广播给前端**
//!      —— 因为 macOS 上非 key window 不分发 mouseMoved 事件，WKWebView 的
//!      CSS `:hover` 在浮窗未聚焦时不会激活，会导致"必须先点一下窗口 hover 才生效"
//!      的体验坑。用 `NSEvent.mouseLocation` + `NSWindow.windowNumberAtPoint`
//!      做命中测试（不仅检查鼠标在 frame 内，还确认浮窗是该点 topmost 窗口），
//!      完全绕过 WebKit 的事件流依赖。
//!
//! ## Hover tracker 生命周期
//!
//! - **始终运行**：lib.rs setup 时调一次 [`start_hover_emitter`]，整个 app 生命
//!   周期不停。idempotent，第二次调用立即返回。
//! - 每 50ms 调 `NSEvent.mouseLocation` + main thread dispatch 拿窗口 frame
//!   做 point-in-rect。开销 ~20Hz 的轻量轮询。
//! - 状态变化时：
//!   - 永远 `app.emit("usticky://floating-hover", inside)` 给前端
//!     （前端拿来切 `body[data-hover]` 属性，驱动 CSS）
//!   - `inside=true` 时**额外** `app.emit("usticky://floating-hover-pos", {x, y})`
//!     给前端拿鼠标坐标走 `elementFromPoint` 找命中哪张 todo-card ——
//!     未聚焦时 WebKit 不派 mouseenter，title-expand 必须靠这条路径
//!   - 当 [`LEVEL_SWITCHING_ACTIVE`] 为 true（PinBottom 模式）时**额外**切 NSWindow level
//!     —— 这是 PinBottom 模式"hover 临时置顶"的实现路径
//!
//! ## 鼠标坐标语义
//!
//! hover-pos 发的是**视口相对坐标**（CSS px，原点 = webview 内容区左上角），
//! 前端拿到直接喂 `elementFromPoint`，不做任何换算。换算在 Rust 端完成：
//! `NSEvent.mouseLocation` 与 `NSWindow.frame` 同坐标系（global screen，
//! bottom-left origin，logical points），直接相减 + 用窗口自身高度翻 Y 轴，
//! 不依赖 tao 单位混用行为 / window.screen 基准屏等易碎假设（2026-07-21 fix）。
//!
//! ## 三个 level 常量
//!
//! - `LEVEL_BELOW_NORMAL = -1` ：在 `kCGNormalWindowLevel` 之下 1 格，所有普通 app
//!   窗口都在我们之上，但我们在桌面背景之上。PinBottom 模式用它。
//! - `LEVEL_FLOATING = 3` ：就是 `kCGFloatingWindowLevel`，相当于 Tauri 的
//!   `set_always_on_top(true)`。PinTop 模式用它，hover 临时置顶也用它。

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use objc2::MainThreadMarker;
use objc2_app_kit::{
    NSApplicationActivationOptions, NSEvent, NSRunningApplication, NSWindow, NSWorkspace,
};
use objc2_core_graphics::{kCGFloatingWindowLevel, kCGNormalWindowLevel, CGWindowLevel};
use objc2_foundation::NSPoint;
use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::todo::PinMode;

/// 始终在底部：`kCGNormalWindowLevel - 1`（= -1）。
///
/// 高于桌面背景（`kCGDesktopWindowLevel`）和 Finder 桌面图标层，
/// 低于所有普通 app 窗口（`kCGNormalWindowLevel` = 0）→ macOS 调度
/// 一直把我们压在最底。AGENTS.md 第 6 节 + Musage 同款决策。
///
/// **2026-07-03 fix**：之前用 `kCGDesktopWindowLevel - 1` 想避开 Sonoma+
/// Dock/Mission Control 偶发遮挡，结果反而把窗口压到 Finder 桌面图标层
/// **之下**，`windowNumberAtPoint` 命中测试永远返回 Finder 桌面窗口而非
/// 浮窗 → `is_floating_topmost_at` 恒 false → PinBottom hover 完全失效。
/// 改回 `-1` 跟 Musage / AGENTS.md 对齐，hover 检测恢复正常。
pub const LEVEL_BELOW_NORMAL: CGWindowLevel = kCGNormalWindowLevel - 1;

/// 始终在顶部：等于 kCGFloatingWindowLevel。
pub const LEVEL_FLOATING: CGWindowLevel = kCGFloatingWindowLevel;

/// hover emitter thread 是否已启动（idempotent 防重入）。
/// 启动后整个 app 生命周期不停，所以这里只是 "thread spawned?" 的标志，
/// 不参与运行时控制 —— 真正想动行为请改 [`LEVEL_SWITCHING_ACTIVE`]。
static TRACKER_RUNNING: AtomicBool = AtomicBool::new(false);

/// **P2-5 fix**：强制下一 tick 强制 emit 一次当前 hover 状态。
///
/// 场景：用户在 PinTop/Normal 模式时鼠标已在浮窗内 → 切到 PinBottom →
/// `set_window_pin_bottom` 调 `set_window_level(BELOW_NORMAL)` 把窗口降下来。
/// 但 hover emitter 的 `last_inside=true`，下一 tick 拿到 raw_inside=true
/// 仍走 `inside == last_inside { continue }` 分支 → 不 emit hover + 不切
/// level → 窗口卡 -1 被盖住。
///
/// `set_window_pin_bottom` 在调 set_window_level 之后置 true，emitter 下一
/// tick 检测到后重置 last_inside 强制走 emit + level 切换分支。一性次
/// 用完即清。
static FORCE_HOVER_EMIT_ONCE: AtomicBool = AtomicBool::new(false);

/// 鼠标 hover 时是否同步切 NSWindow level：仅 PinBottom 模式置 true。
/// 这个开关只影响 level 切换；hover 事件 emit 不受影响（**永远 emit**），
/// 因为前端的 iOS 26 玻璃 hover 效果需要它，不分 pin mode。
static LEVEL_SWITCHING_ACTIVE: AtomicBool = AtomicBool::new(false);

// ── 公开 API ──

/// PinBottom 模式启动时调：把 level 切到 below-normal，并开启 hover 切 level。
/// tracker 已由 [`start_hover_emitter`] 在 app 启动时拉起，这里只翻开关。
pub fn set_window_pin_bottom<R: Runtime>(app: &AppHandle<R>) {
    set_window_level(app, LEVEL_BELOW_NORMAL, false); // 不失焦隐藏，hover 临时置顶
    LEVEL_SWITCHING_ACTIVE.store(true, Ordering::SeqCst);
    // **P2-5 fix**：强制 emitter 下一 tick 重新评估 hover 状态。
    // 详见 FORCE_HOVER_EMIT_ONCE 注释 —— 切 pin mode 时如果鼠标已在
    // 浮窗内，last_inside=true 会让下一 tick 走 inside == last_inside
    // 短路，level 永远停在 -1。
    FORCE_HOVER_EMIT_ONCE.store(true, Ordering::SeqCst);
    start_hover_emitter(app.clone());
}

/// PinTop 模式：level 切到 floating，关闭 hover 切 level（窗口已经始终置顶）。
/// hover 事件 emit 不变，前端的玻璃效果继续受惠。
pub fn set_window_pin_top<R: Runtime>(app: &AppHandle<R>) {
    LEVEL_SWITCHING_ACTIVE.store(false, Ordering::SeqCst);
    set_window_level(app, LEVEL_FLOATING, false); // 不失焦隐藏，"始终置顶"承诺
}

/// Normal 模式：level 切回 0，关闭 hover 切 level。
pub fn set_window_normal<R: Runtime>(app: &AppHandle<R>) {
    LEVEL_SWITCHING_ACTIVE.store(false, Ordering::SeqCst);
    set_window_level(app, kCGNormalWindowLevel, false); // H15 fix: 不失焦隐藏，浮窗始终可见
}

/// hover 切 level 的"前端兜底信号"：macOS 上 tracker 已自行处理，此处 no-op。
/// 保留是为了让 commands.rs 在跨平台调用时不必 `#[cfg]`。
pub fn set_window_hover_raise<R: Runtime>(_app: &AppHandle<R>, _hovering: bool) {
    // no-op —— tracker 自己处理 level 切换
}

/// 未聚焦浮窗的光标反馈：hover 命中操作按钮时前端调 `set_cursor_pointer`
/// 命令切手型，离开切回箭头。
///
/// 为什么需要：非 key window 的 WKWebView 不按 hit-test 元素更新光标
/// （cursor 更新走 mouseMoved 事件流，非 key window 不分发），用户在
/// 未聚焦浮窗上看不到"这个按钮可点"的反馈。`NSCursor.set()` 是进程级
/// 全局设置，非激活 app 调用也立即生效；鼠标离开浮窗后前端在
/// floating-hover(false) 时切回箭头，其他 app 区域的光标由各 app 的
/// mouseMoved 自行管理，互不影响。
pub fn set_cursor_pointer_shape<R: Runtime>(app: &AppHandle<R>, pointer: bool) {
    let _ = app.run_on_main_thread(move || {
        if pointer {
            objc2_app_kit::NSCursor::pointingHandCursor().set();
        } else {
            objc2_app_kit::NSCursor::arrowCursor().set();
        }
    });
}

// ── Quick-add 临时置顶 + 切回原应用 ──
//
// 用户场景：Cmd+Shift+Space 唤出浮窗 → 自动置顶（即便 PinBottom 模式）→
// 焦点进输入框 → 写完按 Esc 或再按一次快捷键 → 浮窗隐藏 + 焦点切回原应用。
//
// 实现要点：
//   1. 唤出时保存当前 frontmost app（用户正在用的应用），便于 dismiss 时切回
//   2. 唤出时把 level 切到 FLOATING（即便 PinBottom），并 disable hover 切 level
//      —— 否则 hover emitter 会跟我们打架（鼠标位置不在浮窗内 → 切回 BELOW_NORMAL）
//   3. dismiss 时把 level + LEVEL_SWITCHING_ACTIVE 按 pin mode 还原
//   4. dismiss 时 activate 保存的 app —— macOS 上 `orderOut:` 不会自动切回原 app

/// 保存"按下快捷键前的 frontmost app"，用于 dismiss 时切回。
///
/// 必须在 `show()` **之前**调 —— 否则 frontmost 就是我们自己。
/// 如果当前 frontmost 就是 Usticky（极少见，比如浮窗已可见但失焦时又按快捷键），
/// 不覆盖已保存的值 —— 保留上次有效保存。
pub fn save_previous_app_for_quick_add() {
    let workspace = NSWorkspace::sharedWorkspace();
    let frontmost = workspace.frontmostApplication();
    let Some(app) = frontmost else { return };
    let current_pid = NSRunningApplication::currentApplication().processIdentifier();
    if app.processIdentifier() == current_pid {
        // 自己是 frontmost —— 不覆盖
        return;
    }
    let mutex = SAVED_PREV_APP.get_or_init(|| Mutex::new(None));
    *mutex.lock().unwrap_or_else(|e| e.into_inner()) = Some(app);
}

/// dismiss 时调：激活之前保存的 app，把焦点切回去。
///
/// macOS 14+ 推荐 `activate()`，但 objc2-app-kit 0.3.2 只暴露了 deprecated 的
/// `activateWithOptions(_:)`。实测在 macOS 26 上仍工作，先用着 —— 后续升级
/// objc2-app-kit 后再切 `activate()`。
pub fn activate_previous_app_after_quick_add() {
    let mutex = SAVED_PREV_APP.get_or_init(|| Mutex::new(None));
    let guard = mutex.lock().unwrap_or_else(|e| e.into_inner());
    let Some(app) = guard.as_ref() else { return };
    // ActivateAllWindows：把目标 app 的所有窗口都拉到前面（vs 只拉 key window）
    let options = NSApplicationActivationOptions::ActivateAllWindows;
    let _ = app.activateWithOptions(options);
    // 不清空 SAVED_PREV_APP —— 下次 quick-add 时 save_previous_app_for_quick_add
    // 会覆盖（如果新 frontmost 不是我们自己）。这样即使 dismiss 后没立刻 save
    // 也保留上次值，行为更稳。
}

/// 保存的"原 frontmost app"。OnceLock<Mutex<Option<Retained<NSRunningApplication>>>>。
/// Retained 是 thread-safe 的（内部 refcount 原子），Mutex 只是串行化访问。
static SAVED_PREV_APP: OnceLock<Mutex<Option<objc2::rc::Retained<NSRunningApplication>>>> =
    OnceLock::new();

/// 唤出浮窗时调：把 level 切到 FLOATING（即便 PinBottom），并 disable hover 切 level。
///
/// 必须**在 show() 之前**调 —— 否则 PinBottom 模式下浮窗先在 -1 显示一帧才升到 3，
/// 视觉上会闪一下被其他 app 盖住的画面。
pub fn raise_for_quick_add<R: Runtime>(app: &AppHandle<R>) {
    // 先 disable hover 切 level —— 防止 raise 后第一个 hover tick 立刻把 level 切回
    LEVEL_SWITCHING_ACTIVE.store(false, Ordering::SeqCst);
    set_window_level(app, LEVEL_FLOATING, false);
}

/// dismiss 时调：按当前 pin mode 还原 level + LEVEL_SWITCHING_ACTIVE。
///
/// 必须在 `hide()` **之后**调 —— 否则 PinBottom 模式下浮窗先从 FLOATING 降到 -1
/// 还显示一帧才隐藏，视觉上会闪一下被其他 app 盖住的画面。
pub fn restore_level_after_quick_add<R: Runtime>(app: &AppHandle<R>, mode: PinMode) {
    match mode {
        PinMode::PinTop => set_window_pin_top(app),
        PinMode::PinBottom => set_window_pin_bottom(app),
        PinMode::Normal => set_window_normal(app),
    }
}

/// 浮窗内鼠标坐标 payload，发到前端驱动 `elementFromPoint`。
///
/// 坐标系 = **视口相对坐标**（CSS px，原点 = webview 内容区左上角），
/// 前端拿到直接喂 `elementFromPoint`，**不需要任何转换**。
///
/// 为什么不做"发屏幕坐标、前端换算"（2026-07-21 fix）：
/// 旧实现发 macOS screen-space 坐标，前端用 `innerPosition()` +
/// `window.screen.height` 手工翻 Y 轴。那条链路叠了三层易碎假设
/// （tao `bottom_left_to_top_left` 的 physical/logical 单位混用、
/// `window.screen` 是窗口所在屏而 mouseLocation 以主屏为基准、
/// Retina scale），任一假设失效就整体错位 —— 实测 cachedWinY 在不同
/// 机器/显示器配置下取值不一致，副屏修复（85dc58a）反而打破主屏。
/// Rust 端同时持有 `NSEvent.mouseLocation` 和 `NSWindow.frame`，两者
/// 同坐标系（global screen，bottom-left origin，logical points），
/// 直接相减才是零假设的稳健做法。
#[derive(serde::Serialize, Clone, Copy)]
struct HoverPos {
    x: f64,
    y: f64,
}

/// 由屏幕坐标（bottom-left origin）+ 窗口 content rect 算视口相对坐标。
/// `frame` = (x, y, w, h)，跟 `NSEvent.mouseLocation` 同坐标系。
fn viewport_hover_pos(mouse: NSPoint, frame: (f64, f64, f64, f64)) -> HoverPos {
    let (fx, fy, _w, fh) = frame;
    HoverPos {
        x: mouse.x - fx,
        // bottom-left origin → top-left origin 翻转，用窗口自身高度，
        // 不依赖任何"屏幕高度"假设。
        y: fh - (mouse.y - fy),
    }
}

/// 启动 hover emitter 线程。idempotent —— 第二次调用立即返回。
/// 由 lib.rs setup() 调一次即可。启动后整个 app 生命周期不停。20Hz 轮询。
///
/// **dwell-time hysteresis**（沿用 Musage 2026-07-03 fix）：
/// 多个 transparent + always-on-top 窗口共存时光标静止在浮窗边缘，
/// `windowNumberAtPoint` 返回值在两个 window number 之间抖；20Hz tick
/// 里 inside 持续翻转 → 每次翻转 emit 一次 → 前端每次都 toggle
/// body[data-hover] → CSS spring 反复起头又被瞬间打断 → 肉眼看到闪。
///
/// 修复：enter 阈值 1 tick（50ms）/ exit 阈值 2 ticks（100ms）。
/// 离开比进入略慢 —— exit 多 1 tick 防 level 切换 stale 振荡；enter
/// 不阈值化，鼠标进入立即触发。
///
/// **2026-07-06 fix**：之前 Usticky 把阈值改成 enter 2 / exit 1 想加速响应，
/// 但 exit=1 太激进 —— PinBottom 模式 hover 临时置顶时，level 切换瞬间
/// `windowNumberAtPoint` 偶发返回 stale 值（窗口 z-order 还没稳定），
/// 单 tick false 就触发 exit → hover 撤销 → 窗口降回 below-normal →
/// 下一 tick 又 inside=true → 重新 enter → 形成 1-2s 周期的振荡，
/// 表现为"毛玻璃效果出现后过一回消失"。改回 Musage 的 3/2 阈值，
/// 给 level 切换留足 z-order 稳定时间，振荡消失。
///
/// **2026-07-06 fix #2**：用户反馈"hover 时间过短时不自动置顶"。
/// 根因：enter=3 (150ms) 阈值下，用户 hover < 150ms 就离开时，
/// `inside` 在 pending_ticks 累计到阈值前就回到与 last_inside 相同
/// 的值（last_inside 仍是 false），命中 `inside == last_inside` 分支
/// → pending_ticks 被重置为 0 → 整个 hover 被 swallowing，enter
/// 永远不触发 → PinBottom 模式下窗口不置顶。
/// 修复：ENTER_THRESHOLD=1（50ms 内即触发）。EXIT_THRESHOLD 保持 2
/// 不动 —— exit 阈值是 level 切换 stale 振荡的关键防线（见上 fix），
/// 不能动。enter=1 副作用是鼠标快速掠过浮窗边缘时可能触发一次
/// "瞬间置顶→降回"，但这正是用户期望的"短 hover 也要置顶"行为。
pub fn start_hover_emitter<R: Runtime>(app: AppHandle<R>) {
    if TRACKER_RUNNING.swap(true, Ordering::SeqCst) {
        return; // 已在跑
    }
    let builder = thread::Builder::new()
        .name("usticky-hover-emitter".into())
        .spawn(move || {
            // **P2-9 fix**：spawn 闭包整体 catch_unwind。
            //
            // 旧实现：只在 builder.spawn() 自身返 Err 时才 reset TRACKER_RUNNING。
            // 线程 loop 内 panic 时（dispatch 闭包野指针 / NSWindow 引用失效等）：
            //   - release profile (panic="abort") → 整个进程 abort
            //   - debug profile (panic="unwind") → 线程静默死掉，TRACKER_RUNNING 永远 true
            //     → 后续 start_hover_emitter 全部 no-op → hover raise / 玻璃效果
            //     永远失效直到重启 app
            //
            // 现在 AssertUnwindSafe 包闭包，panic 时 log + reset TRACKER_RUNNING。
            // 下一轮 start_hover_emitter 就能重新拉起线程恢复功能。
            // 注：catch_unwind 在 panic="abort" profile 下不会生效（abort 优先），
            // 那时本来就会杀进程，所以这是 debug profile 的专属防御。
            let result = catch_unwind(AssertUnwindSafe(|| {
            tracing::debug!("hover emitter 启动");
            let mut last_inside = false;
            // dwell-time hysteresis（沿用 Musage 2026-07-03 fix）：
            // - **enter**：inside=true 必须连续 ≥3 个 tick（150ms）才采纳，
            //   抖动短脉冲被吞。
            // - **exit**：inside=false 必须连续 ≥2 个 tick（100ms）才采纳，略快
            //   因为用户离开时希望玻璃及时撤销（vs enter 多 1 tick 防误触发）。
            // - **enter→exit 切换瞬间 reset 计数器**：避免在过渡中误累计。
            let mut pending_ticks: u8 = 0;
            let mut pending_value = false;

            // **2026-07-03 fix（玻璃 2 秒后丢失问题）**：
            //
            // Usticky v0.1 默认强玻璃（alpha 0.82 + blur 28px always-on），
            // 持续 GPU 高负载。macOS 合成层持续重排 → main thread 持续忙
            // → `is_floating_topmost_at` 的 `run_on_main_thread` 50ms 超时
            // → 持续返 false → "假 exit" → 浮窗玻璃持续 1.5~3 秒丢失。
            //
            // 修复策略：dispatch timeout / 失败时**不立刻采纳 false**，
            // 保留 last_inside。只有当连续 **3 个 tick（150ms）** 都是
            // timeout/失败 才兜底采纳 false。这给 main thread "喘息窗口"，
            // GPU 忙的 1.5~3 秒窗口期不会被假 exit 误触。
            //
            // 为什么 enter 阈值不增加（仍是 1 tick）：
            // - 用户期望"短 hover 也要触发置顶"（2026-07-06 fix 调优过）
            // - 进入浮窗时 GPU 不会突然忙起来（idle 状态低负载），dispatch
            //   几乎一定能 < 50ms 拿到结果，timeout 极少见
            // - 真要 enter timeout 多半是窗口未上屏等真异常，保留即采纳的
            //   语义错误代价低（晚一个 tick 进入）
            //
            // 为什么 fix 在 hover emitter 而不是 is_floating_topmost_at：
            // 改 is_floating_topmost_at 返"未知"会让调用方复杂化；改 emitter
            // 状态机是最小改动，跟现有 dwell-time hysteresis 协同工作。
            let mut consecutive_dispatch_failures: u8 = 0;
            const DISPATCH_FAILURE_THRESHOLD: u8 = 3; // 3 × 50ms = 150ms

            loop {
                thread::sleep(Duration::from_millis(50));

                let mouse = NSEvent::mouseLocation();

                // **P2-5 fix**：检查强制 emit flag。pin mode 切换后置 true，
                // 我们重置 pending_ticks 并让当前 tick 跳过 inside == last_inside
                // 短路 → 真实 inside 状态穿透 emit + level 切换。
                let force_emit = FORCE_HOVER_EMIT_ONCE.swap(false, Ordering::SeqCst);
                if force_emit {
                    pending_ticks = 0;
                    pending_value = false;
                    tracing::debug!("FORCE_HOVER_EMIT_ONCE: 强制下一 tick 重新评估 hover 状态");
                }

                // 关键：用 NSWindow.windowNumberAtPoint 做命中测试 ——
                // 不光检查"鼠标在不在浮窗 frame 内"，还要确认浮窗在该点是**最上层**。
                // **2026-07-03 fix**：当 inside=false 但**已知上次 inside=true 且
                // 当前是 dispatch 失败**，保守认为"可能还在浮窗里" → 不采纳 false。
                // 用 inside_unreliable 替代直接 inside 走下面的状态机。
                let (inside, dispatch_failed, frame) = is_floating_topmost_at_with_status(&app, mouse);

                // dispatch 失败时（main thread 忙 / 窗口未上屏 / MainThreadMarker 不可用），
                // 走"未知"路径：保留 last_inside，不更新 pending_value
                if dispatch_failed {
                    consecutive_dispatch_failures =
                        consecutive_dispatch_failures.saturating_add(1);
                    if consecutive_dispatch_failures >= DISPATCH_FAILURE_THRESHOLD {
                        // 连续 3 tick dispatch 都失败（150ms）→ 兜底采纳 false
                        // （跟 EXIT_THRESHOLD 一致，避免阈值碎裂）
                        if last_inside {
                            last_inside = false;
                            pending_ticks = 0;
                            pending_value = false;
                            if let Err(e) = app.emit("usticky://floating-hover", false) {
                                tracing::trace!(error = %e, "emit hover 失败");
                            }
                            if LEVEL_SWITCHING_ACTIVE.load(Ordering::SeqCst) {
                                set_window_level(&app, LEVEL_BELOW_NORMAL, false);
                            }
                            tracing::debug!(
                                "dispatch 连续失败 {} 次，兜底采纳 false（防永久卡 hover=true）",
                                DISPATCH_FAILURE_THRESHOLD
                            );
                        }
                    }
                    // dispatch 失败期间**不 emit hover-pos**（坐标可能 stale，且
                    // 不希望前端在错误状态下继续切 .todo-card hover）
                    continue;
                }

                consecutive_dispatch_failures = 0; // 重置失败计数

                // **P2-5 fix**：force_emit 时跳过 inside == last_inside 短路，
                // 让本 tick 真实状态穿透 emit + level 切换。一性次，下一 tick
                // last_inside 已经同步回真实值，状态机恢复正常。
                if inside == last_inside && !force_emit {
                    pending_ticks = 0;
                    // inside 没变（持续在浮窗内 or 持续在浮窗外），但若当前
                    // 持续在浮窗内，鼠标坐标可能在卡 A→卡 B 之间移动 →
                    // 每 tick 都发 hover-pos，让前端 elementFromPoint 跟踪命中
                    // 的 todo-card。body[data-hover] 玻璃色只需要 edge trigger，
                    // 不在每 tick emit 避免 CSS spring 反复重置起始点。
                    if inside {
                        if let Some(f) = frame {
                            if let Err(e) = app.emit(
                                "usticky://floating-hover-pos",
                                viewport_hover_pos(mouse, f),
                            ) {
                                tracing::trace!(error = %e, "emit hover-pos 失败");
                            }
                        }
                    }
                    continue;
                }

                // inside 与 last_inside 不同 —— 是真切换还是抖动？
                if pending_value != inside {
                    pending_value = inside;
                    pending_ticks = 1;
                } else {
                    pending_ticks = pending_ticks.saturating_add(1);
                }

                const ENTER_THRESHOLD: u8 = 1; // 50ms —— 短 hover 也要触发置顶
                const EXIT_THRESHOLD: u8 = 2;  // 100ms —— 防 level 切换 stale 振荡
                let threshold = if pending_value { ENTER_THRESHOLD } else { EXIT_THRESHOLD };

                if pending_ticks < threshold {
                    continue;
                }

                // 阈值达成 —— 采纳新状态，emit + 切 level
                last_inside = inside;
                pending_ticks = 0;

                // (1) 永远 emit —— 驱动前端 body[data-hover]，让 CSS hover 生效
                if let Err(e) = app.emit("usticky://floating-hover", inside) {
                    tracing::trace!(error = %e, "emit hover 失败");
                }

                // (2) 进入浮窗时（edge-trigger）补一发 hover-pos，让前端在
                // 未聚焦场景下第一帧就有坐标可喂 elementFromPoint。
                // 持续在浮窗内的卡间切换走上面 if inside { ... continue } 的
                // 每 tick emit。
                if inside {
                    if let Some(f) = frame {
                        if let Err(e) = app.emit(
                            "usticky://floating-hover-pos",
                            viewport_hover_pos(mouse, f),
                        ) {
                            tracing::trace!(error = %e, "emit hover-pos 失败");
                        }
                    }
                }

                // (3) PinBottom 模式：同步切 NSWindow level
                if LEVEL_SWITCHING_ACTIVE.load(Ordering::SeqCst) {
                    let level = if inside {
                        LEVEL_FLOATING
                    } else {
                        LEVEL_BELOW_NORMAL
                    };
                    set_window_level(&app, level, false);
                }
            }
            })); // catch_unwind(inner closure) 收尾
            if let Err(panic_payload) = result {
                let msg = if let Some(s) = panic_payload.downcast_ref::<&str>() {
                    s.to_string()
                } else if let Some(s) = panic_payload.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "unknown panic".to_string()
                };
                tracing::error!("hover emitter thread panic: {}，已 reset TRACKER_RUNNING", msg);
                TRACKER_RUNNING.store(false, Ordering::SeqCst);
            }
        }); // spawn closure 收尾
    // builder.spawn 自身失败（线程创建失败，如 OS 资源耗尽）也要 reset，
    // 不让 start_hover_emitter 永远 no-op。
    if let Err(e) = builder {
        tracing::error!(error = %e, "spawn hover emitter thread 失败，hover raise / glass 效果将失效");
        TRACKER_RUNNING.store(false, Ordering::SeqCst);
    }
}

// ── 内部 ──

/// 把浮窗的 NSWindow level 切到 `level`。dispatch 到 main thread（AppKit 强制要求）。
///
/// `hides_on_deactivate` 参数保留兼容现有调用，但**实际不再生效** —— 所有模式
/// 都硬编码 `setHidesOnDeactivate(false)`（沿用 Musage H15 fix）。
///
/// 原因（Musage 2026-07-03 audit）：之前 Normal/PinTop 模式设 true → app 失焦时
/// 浮窗完全 `hide()`（不是"被遮盖"而是"消失"），违反"始终可见的悬浮窗"产品定义。
/// macOS 普通窗口失焦只是被其他 app 遮盖（level=0 已实现该语义），不是 hide()。
/// 同时失焦 hide 会让 `is_visible()` 返 false → hover emitter 误判 inside=false →
/// 玻璃效果在 app 失焦瞬间错误撤销，是问题 2（hover 后过一回消失）的诱因之一。
pub fn set_window_level<R: Runtime>(
    app: &AppHandle<R>,
    level: CGWindowLevel,
    _hides_on_deactivate: bool,
) {
    let app2 = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(win) = app2.get_webview_window("floating") {
            if let Ok(ptr) = win.ns_window() {
                if !ptr.is_null() {
                    // SAFETY: `ptr` 来自 webview_window 的 NSWindow，整个 app 生命周期有效。
                    let window: &NSWindow = unsafe { &*ptr.cast::<NSWindow>() };
                    window.setLevel(level as _);
                    window.setHidesOnDeactivate(false);
                    // **P2-4 fix**：把 backdrop-refresh emit 挪到闭包内、
                    // setLevel 之后。run_on_main_thread 是**异步**入队的，原
                    // 实现在 dispatch 之前同步 emit → 前端先收到 reflow 触发
                    // paint invalidation → 但此时 WKWebView 的 z-order 还没
                    // 真正变化（dispatch 闭包还没跑）→ reflow 击穿的是旧的
                    // 2s sample 窗口，对新 z-order 完全无效。
                    //
                    // 现在 emit 跟在 setLevel 后面执行（同一 main thread dispatch
                    // 任务，顺序保证）：reflow 触发时 z-order 已经切好，paint
                    // invalidation 击穿的是新的 sample 失效窗口。
                    let _ = app2.emit("usticky://backdrop-refresh", ());
                }
            }
        }
    });
}

/// 命中测试：鼠标在 `point` 处时，浮窗是否是**最上层**窗口。
///
/// 用 `+[NSWindow windowNumberAtPoint:belowWindowWithWindowNumber:]` 传 0
/// （穿透所有 app 检查整个屏幕），返回该点 topmost window 的 ID。
/// 与浮窗自己的 `windowNumber` 比对：
/// - 相等 → 鼠标 hover 在浮窗**可见**部分
/// - 不等 → 别的窗口盖在那里，用户在跟那个窗口交互，不该触发置顶/玻璃显形
///
/// 解决 PinBottom 模式下浮窗被部分遮挡时，鼠标移到被盖的区域也误触发的问题
/// （问题 1），以及由此引发的 JS mouseleave / Rust frame.contains 不一致
/// 导致 hover 状态振荡、玻璃"出现后过一回消失"（问题 2）。
///
/// **2026-07-06 fix**：之前误诊 `windowNumberAtPoint` 会跳过 transparent 窗口
/// 的低 alpha 像素（#app padding / 卡片间隙），改用 `frame.contains` 绕开。
/// 但 `frame.contains` 不检查遮挡 → PinBottom 模式下鼠标在被遮挡区域也
/// 触发 hover=true → 窗口误置顶（问题 1）。实测 `windowNumberAtPoint`
/// 对 non-opaque 窗口仍按 frame 命中（不按 per-pixel alpha），Musage 同款
/// 实现已稳定运行，不存在"transparent 区域命中失效"问题。改回该方案。
///
/// dispatch 到 main thread（NSWindow API 强制要求）。channel 同步等待。
///
/// 返回 `(inside, dispatch_failed, frame)`：
/// - `inside`：浮窗是否是该点 topmost 窗口
/// - `dispatch_failed`：区分"真实判定 false"和"未知"（dispatch 失败/超时/
///   未上屏/MainThreadMarker 不可用）—— 调用方（hover emitter）看到后者
///   按"未知"处理，保留 last_known_inside。
/// - `frame`：浮窗 content rect，(x, y, w, h)，与 `NSEvent.mouseLocation`
///   同坐标系（global screen，bottom-left origin，logical points）。
///   hover-pos 的视口相对坐标换算就靠它（见 `viewport_hover_pos`）。
///   dispatch 失败时为 None。
fn is_floating_topmost_at_with_status<R: Runtime>(
    app: &AppHandle<R>,
    point: NSPoint,
) -> (bool, bool, Option<(f64, f64, f64, f64)>) {
    use std::sync::{Arc, Condvar, Mutex};

    struct OneSlot {
        slot: Mutex<Option<(bool, bool, Option<(f64, f64, f64, f64)>)>>, // (inside, dispatch_failed, frame)
        cvar: Condvar,
    }
    static SLOT: OnceLock<Arc<OneSlot>> = OnceLock::new();
    let slot = SLOT.get_or_init(|| {
        Arc::new(OneSlot {
            slot: Mutex::new(None),
            cvar: Condvar::new(),
        })
    });

    let app2 = app.clone();
    let slot2 = slot.clone();
    // **P1-3 fix**：dispatch 之前显式清空 slot。
    //
    // 旧实现 wait 循环只在 slot 是 None 时阻塞，slot 一旦被任意一次
    // dispatch 写入 (inside, false) 后就**永远是 Some**，后续 wait 立即
    // 返回上次的 stale 结果。极端场景下：main thread 被 GPU 高负载卡住
    // 数百 ms，dispatch 闭包排队但不执行 → slot 仍持有上一次成功 dispatch
    // 的 `(true, false)`（鼠标之前在窗内）→ hover emitter 收到的 inside
    // 永远跟 stale 值一致 → consecutive_dispatch_failures 永远不递增
    // → "3 tick dispatch 失败兜底采纳 false" 防御**永远不触发** → 鼠标
    // 实际移出后 PinBottom 窗口卡在 FLOATING / 玻璃持续亮。
    //
    // 修复：dispatch 前清空 slot 让 wait 阻塞必须等到新 dispatch 写入。
    // dispatch 失败 / 走 None 分支（main thread dispatch 报错）会兜底写入
    // `(false, true)`，超时分支（50ms 内 dispatch 没跑完）会保持 None，
    // wait 超时返 `(false, true)` 兜底。两条兜底路径都让 hover emitter
    // 走"未知"路径而不是 stale "inside=true"。
    {
        let mut g = slot.slot.lock().unwrap_or_else(|e| e.into_inner());
        *g = None;
    }
    let dispatch_result = app.run_on_main_thread(move || {
        let result = (|| -> Option<(bool, (f64, f64, f64, f64))> {
            let win = app2.get_webview_window("floating")?;
            let ptr = win.ns_window().ok()?;
            if ptr.is_null() {
                return None;
            }
            // SAFETY: ptr 来自 webview_window 的 NSWindow，整个 app 生命周期有效。
            let window: &NSWindow = unsafe { &*ptr.cast::<NSWindow>() };
            let our_id = window.windowNumber();
            if our_id == 0 {
                // 窗口还没上屏（极少见，初始化竞态）→ 当作 dispatch 失败
                return None;
            }
            let Some(mtm) = MainThreadMarker::new() else {
                tracing::trace!("is_floating_topmost_at: MainThreadMarker 不可用，跳过本 tick");
                return None;
            };
            let topmost = NSWindow::windowNumberAtPoint_belowWindowWithWindowNumber(point, 0, mtm);
            // content rect（webview 区域）与 mouseLocation 同坐标系，
            // 前端 hover-pos 换算成视口相对坐标的基准。
            let content = window.contentRectForFrameRect(window.frame());
            let frame = (
                content.origin.x,
                content.origin.y,
                content.size.width,
                content.size.height,
            );
            Some((topmost == our_id, frame))
        })();
        // (inside, dispatch_failed, frame) 区分"真实判定"和"未知"：
        //   - Some((inside, frame))     → (inside, false, Some(frame))
        //   - None（窗口未上屏 / MTM 不可用）→ (false, true, None)
        let payload = match result {
            Some((inside, frame)) => (inside, false, Some(frame)),
            None => (false, true, None),
        };
        {
            let mut g = slot2.slot.lock().unwrap_or_else(|e| e.into_inner());
            *g = Some(payload);
        }
        slot2.cvar.notify_all();
    });
    if let Err(e) = dispatch_result {
        tracing::trace!(
            error = %e,
            "is_floating_topmost_at: dispatch to main thread 失败，立即返 (false, true, None)"
        );
        {
            let mut g = slot.slot.lock().unwrap_or_else(|e| e.into_inner());
            if g.is_none() {
                *g = Some((false, true, None));
            }
        }
        slot.cvar.notify_all();
    }

    // 50ms 超时兜底：main thread 卡住时 hover 轮询不至于一起卡住
    let started = std::time::Instant::now();
    let deadline = Duration::from_millis(50);
    let mut guard = slot.slot.lock().unwrap_or_else(|e| e.into_inner());
    while guard.is_none() && started.elapsed() < deadline {
        let remaining = deadline.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            break;
        }
        let (g, _wait_timeout) = slot
            .cvar
            .wait_timeout(guard, remaining)
            .unwrap_or_else(|e| e.into_inner());
        guard = g;
    }
    // 超时仍 None → (false, true, None) 兜底（dispatch 失败的语义，让 hover emitter 走"未知"路径）
    guard.unwrap_or((false, true, None))
}