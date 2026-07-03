// 平台特定代码 —— 跨平台 pin mode（pin_top / pin_bottom / normal）
//
// macOS: NSWindow.setLevel(-1/3) + NSEvent.mouseLocation 全局轮询 + 主线程
//        dispatch。详见 platform/macos.rs 顶部 doc comment。
// Win:   Win32 z-order 平铺列表，OS 焦点调度持续 demote，无法稳定压住。
//        50ms tick + dual-path（SetWindowLongW + SetWindowPos）best-effort。
//        详见 platform/windows.rs 顶部 doc comment。
// Linux: 暂未实现 —— musage 也没做。等有用户报 bug 再写。

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "windows")]
pub mod windows;

// Linux: 暂未实现 —— musage 也没做。等有用户报 bug 再写。
// linux.rs 已删除（v0.1 不做任何 Linux 私有 API stub；plat 模块本身已是 no-op）

// ── 跨平台统一 API ──
//
// commands.rs 调这些函数，不写 #[cfg]。每个平台自己实现匹配签名即可。
pub use self::plat::*;

#[cfg(target_os = "macos")]
mod plat {
    pub use super::macos::*;
}

#[cfg(target_os = "windows")]
mod plat {
    pub use super::windows::*;
}

#[cfg(target_os = "linux")]
mod plat {
    // Linux stub —— 跟 Win 一样的形态，但是 no-op。
    // Tauri 的 set_always_on_top(true) 走 GTK Wayland/X11 走 WM_WINDOW_ROLE，
    // 没私有 API 可调，alwaysOnTop: true 已经是最实用的方案。
    use tauri::{AppHandle, Runtime};
    pub fn set_window_pin_top<R: Runtime>(_app: &AppHandle<R>) {}
    pub fn set_window_pin_bottom<R: Runtime>(_app: &AppHandle<R>) {}
    pub fn set_window_normal<R: Runtime>(_app: &AppHandle<R>) {}
    pub fn set_window_hover_raise<R: Runtime>(_app: &AppHandle<R>, _hovering: bool) {}
    pub fn start_hover_emitter<R: Runtime>(_app: AppHandle<R>) {}
}