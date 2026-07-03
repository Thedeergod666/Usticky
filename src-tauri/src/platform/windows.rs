//! Windows 端 PinBottom 模式"hover 临时置顶"实现（best-effort）。
//!
//! ## 设计原则
//!
//! Rust 后台线程轮询全局鼠标位置 + 浮窗屏幕 rect，对照 macOS 那套
//! `NSEvent.mouseLocation` + `NSWindow.windowNumberAtPoint` 的形态。
//! 50ms tick（~20Hz）单次调用 ~微秒级 Win32 API 开销。
//!
//! ## 为什么不在 Win 走 JS 路径
//!
//! 早期版本让前端 JS 在 `document.body` 上挂 `mouseenter` / `mouseleave`
//! 然后 `set_always_on_top` 切换。Win + WebView2 + 透明窗上有两个坑：
//!
//! 1. `mouseleave` 在 transparent window 上不可靠 —— body 是
//!    `background: transparent`（来自 styles.css），Chromium 对透明区域的
//!    鼠标命中测试有时不记事件，鼠标快速移出 + 切焦点会丢 leave。CSS
//!    玻璃 hover 有 Rust emit 兜底，但 IPC 链路靠 mouseleave 触发 → 状态机
//!    卡死。
//!
//! 2. `WS_EX_TOPMOST` 出生残留 —— tauri.conf.json 浮窗 `alwaysOnTop:
//!    true` 让窗口**创建时**就带 WS_EX_TOPMOST。后续 `SetWindowPos(
//!    HWND_NOTOPMOST)` 取消 topmost 在部分 Win10/11 上保留 topmost 行为。
//!    Usticky 决策：保留 `alwaysOnTop: true` 作默认 + 提供 pin 模式手动切换，
//!    在 `set_window_normal` 路径显式 AND-out `WS_EX_TOPMOST` 兜底（见下）。
//!
//! ## hit test —— "未被遮挡"才算（macOS-parity）
//!
//! 单纯 `point_in_rect` 太宽松：浮窗 frame 被其它 app 部分盖住时，鼠标
//! 移到被盖区域（用户其实在跟那个 app 交互）会误触发 raise。Win 端用
//! `WindowFromPoint(pt)` 拿 topmost window 的 hwnd，**沿 parent 链
//! 爬到顶层根**（`GetAncestor(_, GA_ROOT)`）后必须等于浮窗自己 —— 严格
//! 只算"浮窗是最上层"的那一格。
//!
//! WebView2 是浮窗的子窗口，`WindowFromPoint` 在浮窗可见区域返回的是
//! WebView2 的 hwnd（不是浮窗的）。`GetAncestor(WebView2, GA_ROOT)` 沿
//! parent 链爬到顶层根（就是我们的浮窗），比对通过。
//!
//! ## Win 端 z-order 是 best-effort
//!
//! `HWND_TOPMOST` 是个**位置**，不是 macOS 那套 `NSWindow level` 那种
//! 有 window server 持久维持的**级别**。WebView2 / OS 焦点调度 / DWM
//! 合成会**持续** demote `WS_EX_TOPMOST` style bit，user space 没有稳
//! 定压制的路径。50ms tick + dual-path（`SetWindowLongW` 直接改 style
//! bit + `SetWindowPos` 走 z-order API）走 best-effort。
//!
//! 焦点丢失（用户点别处 app）后 hover-raise 大概率**不**生效 —— 端用户
//! 可点 tray 菜单 "强制置顶浮窗" 走更暴力的路径
//! （`AllowSetForegroundWindow(ASFW_ANY) + SetForegroundWindow`），
//! 代价是抢焦点。
//!
//! ## Hover tracker 生命周期
//!
//! - 始终运行，由 `start_hover_emitter` 拉起一次
//! - 50ms tick，每 tick：
//!   1. 永远 `app.emit("usticky://floating-hover", inside)` 给前端
//!      （驱动 CSS `body[data-hover]` 玻璃效果）
//!   2. 当 `LEVEL_SWITCHING_ACTIVE` 为 true（PinBottom 模式）：
//!      - `inside == true` → re-assert `HWND_TOPMOST`
//!      - `inside` 切到 `false`（edge-trigger）→ drop 到 `HWND_BOTTOM`

use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager, Runtime};
use windows_sys::Win32::Foundation::GetLastError;
use windows_sys::Win32::Foundation::{HWND as WIN_HWND, POINT, RECT};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetAncestor, GetCursorPos, GetWindowLongPtrW, GetWindowRect, SetWindowLongPtrW, SetWindowPos,
    WindowFromPoint, GA_ROOT, GWLP_EXSTYLE, HWND_BOTTOM, HWND_NOTOPMOST, HWND_TOPMOST,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, WS_EX_TOPMOST,
};

/// Hover tracker thread 是否已启动（idempotent 防重入）。
static TRACKER_RUNNING: AtomicBool = AtomicBool::new(false);

/// 鼠标 hover 时是否同步切 z-order：仅 PinBottom 模式置 true。
static LEVEL_SWITCHING_ACTIVE: AtomicBool = AtomicBool::new(false);

/// 浮窗的 z-order 模式。
#[derive(Debug, Clone, Copy)]
enum ZOrder {
    /// `HWND_TOPMOST` —— 高于所有其它窗口。PinTop 模式 + PinBottom hover 时用。
    TopMost,
    /// `HWND_BOTTOM` —— 低于所有 normal 窗口。PinBottom 模式 + hover 出窗口时用。
    ///
    /// 为什么不直接用 `set_always_on_top(false)`（即 HWND_NOTOPMOST）：
    /// 后者只把 HWND 的 WS_EX_TOPMOST 标志位清掉，**不动 z-order**。
    /// 浮窗之前在 topmost 位置，清掉 topmost 标志后会落回 "top of
    /// normal z-order"，**视觉上还是盖在其它 app 之上**。HWND_BOTTOM
    /// 是显式"塞到正常 z-order 最底"，跟 macOS `LEVEL_BELOW_NORMAL` 对齐。
    Bottom,
    /// `HWND_NOTOPMOST` —— 清 topmost 标志、保留 z-order。Normal 模式用。
    NotTopMost,
}

/// 把浮窗的 z-order 设到指定模式。**双路并发 re-assert**：
/// - **路 A**：`SetWindowPos(HWND_TOPMOST, ...)` —— 标准 z-order 操纵
/// - **路 B**：`SetWindowLongW(GWL_EXSTYLE, ex | WS_EX_TOPMOST)` + 紧跟
///   `SetWindowPos` flush cache —— 直接改 style bit
///
/// `SetWindowLongW` **必须 OR 不能替换** —— 直接 `0x0008` 会
/// wipe 掉 `WS_EX_LAYERED` / `WS_EX_NOREDIRECTIONBITMAP` 等所有 bit。
unsafe fn apply_z_order(hwnd: *mut core::ffi::c_void, z: ZOrder) {
    let insert_after = match z {
        ZOrder::TopMost => HWND_TOPMOST,
        ZOrder::Bottom => HWND_BOTTOM,
        ZOrder::NotTopMost => HWND_NOTOPMOST,
    };

    // 路 B：直接改 style bit
    // 64-bit 进程必须用 GetWindowLongPtrW / SetWindowLongPtrW（返回 LONG_PTR = i64），
    // 避免 LONG (i32) 截断。windows-sys 0.59 把两者放在同一 feature gate。
    match z {
        ZOrder::TopMost => {
            let ex_style = GetWindowLongPtrW(hwnd, GWLP_EXSTYLE);
            let new_style: isize = ex_style | (WS_EX_TOPMOST as isize);
            SetWindowLongPtrW(hwnd, GWLP_EXSTYLE, new_style);
        }
        ZOrder::Bottom | ZOrder::NotTopMost => {
            // Bottom + NotTopMost 都显式 AND-out WS_EX_TOPMOST。
            // WebView2 会在自己的 message handler 里 re-assert WS_EX_TOPMOST，
            // 不显式清的话 Normal 模式在 Win10/11 上不可靠。
            let ex_style = GetWindowLongPtrW(hwnd, GWLP_EXSTYLE);
            let new_style: isize = ex_style & !((WS_EX_TOPMOST as isize));
            SetWindowLongPtrW(hwnd, GWLP_EXSTYLE, new_style);
        }
    }

    // 路 A：z-order API + flush 路 B 的 cache
    SetWindowPos(
        hwnd,
        insert_after,
        0,
        0,
        0,
        0,
        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
    );
}

// ── 公开 API ──

/// PinBottom 模式启动时调：把窗口塞到 `HWND_BOTTOM`，并开启 hover 切 z-order。
///
/// **L10 fix（沿用 Musage 经验）**：先把 `LEVEL_SWITCHING_ACTIVE` 置 true 再 dispatch
/// 闭包。新顺序保证 observer 看到的 store 永远先于或与 z-order 切换同时生效。
pub fn set_window_pin_bottom<R: Runtime>(app: &AppHandle<R>) {
    LEVEL_SWITCHING_ACTIVE.store(true, Ordering::SeqCst);
    let app2 = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(win) = app2.get_webview_window("floating") {
            if let Ok(hwnd) = win.hwnd() {
                unsafe { apply_z_order(hwnd.0, ZOrder::Bottom) };
            }
        }
    });
    start_hover_emitter(app.clone());
}

/// PinTop 模式：z-order 切到 `TopMost`，关闭 hover 切换。
pub fn set_window_pin_top<R: Runtime>(app: &AppHandle<R>) {
    LEVEL_SWITCHING_ACTIVE.store(false, Ordering::SeqCst);
    let app2 = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(win) = app2.get_webview_window("floating") {
            if let Ok(hwnd) = win.hwnd() {
                unsafe { apply_z_order(hwnd.0, ZOrder::TopMost) };
            }
        }
    });
}

/// Normal 模式：z-order 切到 `NotTopMost`（清 topmost 标志、保留 z-order），
/// 关闭 hover 切换。
pub fn set_window_normal<R: Runtime>(app: &AppHandle<R>) {
    LEVEL_SWITCHING_ACTIVE.store(false, Ordering::SeqCst);
    let app2 = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(win) = app2.get_webview_window("floating") {
            if let Ok(hwnd) = win.hwnd() {
                unsafe { apply_z_order(hwnd.0, ZOrder::NotTopMost) };
            }
        }
    });
}

/// hover 切 z-order 的"前端兜底信号"：Win 上 tracker 已自行处理，此处 no-op。
/// 保留是为了让 commands.rs 在跨平台调用时不必 `#[cfg]`。
pub fn set_window_hover_raise<R: Runtime>(_app: &AppHandle<R>, _hovering: bool) {
    // no-op —— tracker 自己处理
}

/// 启动 hover emitter 线程。idempotent —— 第二次调用立即返回。
pub fn start_hover_emitter<R: Runtime>(app: AppHandle<R>) {
    if TRACKER_RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    let builder = thread::Builder::new()
        .name("usticky-hover-emitter".into())
        .spawn(move || {
            let mut last_inside = false;
            loop {
                thread::sleep(Duration::from_millis(50));

                let Some(inside) = is_cursor_inside_floating(&app) else {
                    continue;
                };

                // (1) 永远 emit hover 事件（驱动 CSS 玻璃效果）
                if inside != last_inside {
                    let _ = app.emit("usticky://floating-hover", inside);
                }

                // (2) PinBottom 模式：切 z-order
                if LEVEL_SWITCHING_ACTIVE.load(Ordering::SeqCst) {
                    if inside {
                        // inside: 每 tick re-assert TopMost（best-effort）
                        if let Some(win) = app.get_webview_window("floating") {
                            if let Ok(hwnd) = win.hwnd() {
                                unsafe { apply_z_order(hwnd.0, ZOrder::TopMost) };
                            }
                        }
                    } else if last_inside {
                        // 刚离开: edge-trigger drop 到 BOTTOM
                        if let Some(win) = app.get_webview_window("floating") {
                            if let Ok(hwnd) = win.hwnd() {
                                unsafe { apply_z_order(hwnd.0, ZOrder::Bottom) };
                            }
                        }
                    }
                }

                last_inside = inside;
            }
        });
    if let Err(e) = builder {
        tracing::error!(error = %e, "spawn hover emitter thread 失败，hover raise / glass 效果将失效");
        TRACKER_RUNNING.store(false, Ordering::SeqCst);
    }
}

// ── 内部 ──

/// Hit test：鼠标位置是否在浮窗**未遮挡**区域内。
///
/// 严格判定两步（macOS-parity，对应 `windowNumberAtPoint`）：
/// 1. 鼠标在浮窗 rect 内（`GetWindowRect` → `point_in_rect`）
/// 2. 鼠标该点 topmost window 沿 parent 链爬到顶层根（`GetAncestor(_, GA_ROOT)`）
///    后必须等于浮窗自己 —— 防止"被另一个 app 窗口盖住时误触发 raise"
///
/// 返回 `None` 表示本轮无法判定（窗口未上屏 / Win API 失败），caller
/// continue 即可。
fn is_cursor_inside_floating<R: Runtime>(app: &AppHandle<R>) -> Option<bool> {
    let win = app.get_webview_window("floating")?;
    let hwnd_t = win.hwnd().ok()?;
    if hwnd_t.0.is_null() {
        return None;
    }
    let our_hwnd: *mut core::ffi::c_void = hwnd_t.0;

    // SAFETY: GetCursorPos / GetWindowRect / WindowFromPoint / GetAncestor /
    // GetLastError 都是 Win32 kernel call，文档明确 thread-safe。
    unsafe {
        let mut pt: POINT = std::mem::zeroed();
        if GetCursorPos(&mut pt) == 0 {
            let err = GetLastError();
            tracing::trace!(
                error = err,
                "is_cursor_inside_floating: GetCursorPos 失败,跳过本 tick"
            );
            return None;
        }
        let mut rect: RECT = std::mem::zeroed();
        if GetWindowRect(our_hwnd, &mut rect) == 0 {
            let err = GetLastError();
            tracing::trace!(
                error = err,
                "is_cursor_inside_floating: GetWindowRect 失败,跳过本 tick"
            );
            return None;
        }
        if !point_in_rect(pt, &rect) {
            return Some(false);
        }
        // WindowFromPoint 成功 → topmost non-null = 真实命中窗口。
        // 失败 → null = 当作"未被浮窗遮挡",返 false (保守不 raise)。
        let topmost: WIN_HWND = WindowFromPoint(pt);
        if topmost.is_null() {
            return Some(false);
        }
        let root = GetAncestor(topmost, GA_ROOT);
        if root.is_null() {
            return Some(topmost == our_hwnd);
        }
        Some(root == our_hwnd)
    }
}

#[inline]
fn point_in_rect(pt: POINT, rect: &RECT) -> bool {
    pt.x >= rect.left && pt.x < rect.right && pt.y >= rect.top && pt.y < rect.bottom
}