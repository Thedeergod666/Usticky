//! macOS 特定：两件事
//!
//!   1. 把浮窗的 NSWindow level 直接设到非 0 位置，实现"始终置底/置顶"。
//!   2. **全局鼠标位置轮询，把 hover 状态广播给前端**
//!      —— 因为 macOS 上非 key window 不分发 mouseMoved 事件，WKWebView 的
//!      CSS `:hover` 在浮窗未聚焦时不会激活，会导致"必须先点一下窗口 hover 才生效"
//!      的体验坑。用 `NSEvent.mouseLocation` + 窗口 frame 做 point-in-rect 判断，
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
//!   - 当 [`LEVEL_SWITCHING_ACTIVE`] 为 true（PinBottom 模式）时**额外**切 NSWindow level
//!     —— 这是 PinBottom 模式"hover 临时置顶"的实现路径
//!
//! ## 三个 level 常量
//!
//! - `LEVEL_BELOW_NORMAL = -1` ：在 `kCGNormalWindowLevel` 之下 1 格，所有普通 app
//!   窗口都在我们之上，但我们在桌面背景之上。PinBottom 模式用它。
//! - `LEVEL_FLOATING = 3` ：就是 `kCGFloatingWindowLevel`，相当于 Tauri 的
//!   `set_always_on_top(true)`。PinTop 模式用它，hover 临时置顶也用它。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::thread;
use std::time::Duration;

use objc2::MainThreadMarker;
use objc2_app_kit::{NSEvent, NSWindow};
use objc2_core_graphics::{kCGFloatingWindowLevel, kCGNormalWindowLevel, CGWindowLevel};
use objc2_foundation::NSPoint;
use tauri::{AppHandle, Emitter, Manager, Runtime};

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
    set_window_level(app, kCGNormalWindowLevel, true); // 失焦隐藏，跟普通窗口一致
}

/// hover 切 level 的"前端兜底信号"：macOS 上 tracker 已自行处理，此处 no-op。
/// 保留是为了让 commands.rs 在跨平台调用时不必 `#[cfg]`。
pub fn set_window_hover_raise<R: Runtime>(_app: &AppHandle<R>, _hovering: bool) {
    // no-op —— tracker 自己处理 level 切换
}

/// 启动 hover emitter 线程。idempotent —— 第二次调用立即返回。
/// 由 lib.rs setup() 调一次即可。启动后整个 app 生命周期不停。
pub fn start_hover_emitter<R: Runtime>(app: AppHandle<R>) {
    if TRACKER_RUNNING.swap(true, Ordering::SeqCst) {
        return; // 已在跑
    }
    let builder = thread::Builder::new()
        .name("usticky-hover-emitter".into())
        .spawn(move || {
            tracing::debug!("hover emitter 启动");
            let mut last_emitted = false;
            // v2（2026-07-03）：去掉了 150ms 防抖（DEBOUNCE_TICKS=3）。
            // 旧防抖是为了避免边缘抖动时 backdrop-filter 28px 反复重合成闪烁，
            // 但 v2 把玻璃材质常驻强开后，hover 只切 color / text-shadow
            // （CSS variable swap），这些属性的前端 transition（.todo-title
            // 0.28s color 等）已经能吸收抖动 —— color 过渡中反复方向只是
            // "颜色微微抖动"，不像 backdrop-filter 反复重合成那样刺眼。
            // 前端 main.ts 的 setHoverAttr 还用 rAF 合并同帧多次 emit，
            // 进一步降低 DOM 写入频率。
            // 净效果：50ms tick 检测到就立即 emit，进入/离开都无感知延迟。

            loop {
                thread::sleep(Duration::from_millis(50));

                let mouse = NSEvent::mouseLocation();

                // 关键：用 NSWindow.windowNumberAtPoint 做命中测试 ——
                // 不光检查"鼠标在不在浮窗 frame 内"，还要确认浮窗在该点是**最上层**。
                let inside = is_floating_topmost_at(&app, mouse);

                if inside != last_emitted {
                    last_emitted = inside;

                    // (1) 永远 emit —— 驱动前端 body[data-hover]，让 CSS hover 生效
                    if let Err(e) = app.emit("usticky://floating-hover", inside) {
                        tracing::trace!(error = %e, "emit hover 失败");
                    }

                    // (2) PinBottom 模式：同步切 NSWindow level
                    if LEVEL_SWITCHING_ACTIVE.load(Ordering::SeqCst) {
                        let level = if inside {
                            LEVEL_FLOATING
                        } else {
                            LEVEL_BELOW_NORMAL
                        };
                        set_window_level(&app, level, false);
                    }
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
/// `hides_on_deactivate` 控制 setHidesOnDeactivate —— 失焦时是否隐藏。
/// - Normal 模式：传 true（跟普通窗口行为一致）
/// - PinTop 模式：传 false（"始终置顶"的承诺 = 失焦也得在）
/// - PinBottom 模式：传 false（鼠标 hover 临时置顶，要求浮窗随时可达）
pub fn set_window_level<R: Runtime>(
    app: &AppHandle<R>,
    level: CGWindowLevel,
    hides_on_deactivate: bool,
) {
    let app2 = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(win) = app2.get_webview_window("floating") {
            if let Ok(ptr) = win.ns_window() {
                if !ptr.is_null() {
                    // SAFETY: `ptr` 来自 webview_window 的 NSWindow，整个 app 生命周期有效。
                    let window: &NSWindow = unsafe { &*ptr.cast::<NSWindow>() };
                    window.setLevel(level as _);
                    window.setHidesOnDeactivate(hides_on_deactivate);
                }
            }
        }
    });
}

/// 命中测试：鼠标在 `point` 处时，浮窗是否是**最上层**窗口。
///
/// 用 `+[NSWindow windowNumberAtPoint:belowWindowWithWindowNumber:]` 传 0
/// （穿透所有 app 检查整个屏幕），返回该点 topmost window 的 ID。
/// 与浮窗自己的 `windowNumber` 比对，相等则 hover 在浮窗**可见**部分。
///
/// 全局复用 `std::sync::Mutex<Option<bool>>` + `Condvar` 单槽位
/// （外层包 `OnceLock<Arc<...>>` 复用），避免 20Hz × 24h 的 allocator churn。
///
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
                return Some(false);
            }
            let Some(mtm) = MainThreadMarker::new() else {
                tracing::trace!("is_floating_topmost_at: MainThreadMarker 不可用，跳过本 tick");
                return Some(false);
            };
            let topmost = NSWindow::windowNumberAtPoint_belowWindowWithWindowNumber(point, 0, mtm);
            Some(topmost == our_id)
        })();
        // 写结果前先清空旧值 —— 避免读到上一次的 stale 值
        // （call loop 的 while guard.is_none() 直接跳过、返回旧结果）。
        {
            let mut g = slot2.slot.lock().unwrap_or_else(|e| e.into_inner());
            *g = Some(result.unwrap_or(false));
        }
        slot2.cvar.notify_all();
    });
    // 主线程无法调度 (临时忙 / 退出中) —— 立即把 slot 填 false 让
    // cvar notify_all 提前返 poll 路径，避免调用方空等 50ms。
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