// 平台特定代码 —— v0.1 stub
//
// Musage 的 platform/macos.rs 实现了 PinBottom（NSWindow.setLevel(-1)）+ hover
// emitter（NSEvent.mouseLocation 全局轮询）。Usticky v0.1 不做：
//   - alwaysOnTop: true（默认置顶，先看用户对"遮挡其它 app"的容忍度）
//   - PinBottom / hover emitter v2+ 再考虑
//   - Win 端悬停 tracker v2+ 再考虑
//
// 这层留空 stub 是为了让 Cargo 在所有平台都能编译（macos-private-api feature
// 在非 macOS 平台编译时跳过），同时给未来留好钩子位置。

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "windows")]
pub mod windows;

#[cfg(target_os = "linux")]
pub mod linux;