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
//! 发的坐标是 macOS screen-space logical points（NSEvent.mouseLocation 原生返回，
//! bottom-left origin）。前端要做 Y 翻转（macOS 是 bottom-left，CSS 是 top-left）
//! 再喂 `elementFromPoint`。单位已经是 logical（CSS px），前端不需要再除 dpr。
//!
//! ## 三个 level 常量
//!
//! - `LEVEL_BELOW_NORMAL = -1` ：在 `kCGNormalWindowLevel` 之下 1 格，所有普通 app
//!   窗口都在我们之上，但我们在桌面背景之上。PinBottom 模式用它。
//! - `LEVEL_FLOATING = 3` ：就是 `kCGFloatingWindowLevel`，相当于 Tauri 的
//!   `set_always_on_top(true)`。PinTop 模式用它，hover 临时置顶也用它。

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
/// 坐标系 = macOS screen-space logical points（= CSS px，前端不再除 dpr）。
/// macOS 是 bottom-left origin，前端收到要做 Y 翻转才能喂 elementFromPoint。
#[derive(serde::Serialize, Clone, Copy)]
struct HoverPos {
    x: f64,
    y: f64,
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
            loop {
                thread::sleep(Duration::from_millis(50));

                let mouse = NSEvent::mouseLocation();

                // 关键：用 NSWindow.windowNumberAtPoint 做命中测试 ——
                // 不光检查"鼠标在不在浮窗 frame 内"，还要确认浮窗在该点是**最上层**。
                let inside = is_floating_topmost_at(&app, mouse);

                if inside == last_inside {
                    pending_ticks = 0;
                    // inside 没变（持续在浮窗内 or 持续在浮窗外），但若当前
                    // 持续在浮窗内，鼠标坐标可能在卡 A→卡 B 之间移动 →
                    // 每 tick 都发 hover-pos，让前端 elementFromPoint 跟踪命中
                    // 的 todo-card。body[data-hover] 玻璃色只需要 edge trigger，
                    // 不在每 tick emit 避免 CSS spring 反复重置起始点。
                    if inside {
                        if let Err(e) = app.emit(
                            "usticky://floating-hover-pos",
                            HoverPos { x: mouse.x, y: mouse.y },
                        ) {
                            tracing::trace!(error = %e, "emit hover-pos 失败");
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
                    if let Err(e) = app.emit(
                        "usticky://floating-hover-pos",
                        HoverPos { x: mouse.x, y: mouse.y },
                    ) {
                        tracing::trace!(error = %e, "emit hover-pos 失败");
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
        });
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
/// 拿不到 / 超时 / 浮窗未上屏 → 保守返回 false。
fn is_floating_topmost_at<R: Runtime>(app: &AppHandle<R>, point: NSPoint) -> bool {
    use std::sync::{Arc, Condvar, Mutex};

    struct OneSlot {
        slot: Mutex<Option<bool>>,
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
    let dispatch_result = app.run_on_main_thread(move || {
        let result = (|| -> Option<bool> {
            let win = app2.get_webview_window("floating")?;
            let ptr = win.ns_window().ok()?;
            if ptr.is_null() {
                return None;
            }
            // SAFETY: ptr 来自 webview_window 的 NSWindow，整个 app 生命周期有效。
            let window: &NSWindow = unsafe { &*ptr.cast::<NSWindow>() };
            let our_id = window.windowNumber();
            if our_id == 0 {
                // 窗口还没上屏（极少见，初始化竞态）→ 直接 false
                return Some(false);
            }
            // 传 0 = 不排除任何窗口，返回整个屏幕在该点 topmost window 的 number
            let Some(mtm) = MainThreadMarker::new() else {
                return Some(false);
            };
            let topmost = NSWindow::windowNumberAtPoint_belowWindowWithWindowNumber(point, 0, mtm);
            Some(topmost == our_id)
        })();
        {
            let mut g = slot2.slot.lock().unwrap_or_else(|e| e.into_inner());
            *g = Some(result.unwrap_or(false));
        }
        slot2.cvar.notify_all();
    });
    if let Err(e) = dispatch_result {
        tracing::trace!(
            error = %e,
            "is_floating_topmost_at: dispatch to main thread 失败，立即返 false"
        );
        {
            let mut g = slot.slot.lock().unwrap_or_else(|e| e.into_inner());
            if g.is_none() {
                *g = Some(false);
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
    guard.unwrap_or(false)
}