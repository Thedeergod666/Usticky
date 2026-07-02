// Windows stub —— v0.1 不实现 hover-raise。
// Musage 实战结论：Win32 z-order 是平铺列表，OS 焦点调度持续 demote，无法
// 稳定压住。v2+ 也不打算做。浮窗 alwaysOnTop: true 已是最实用的方案。

#[allow(dead_code)]
pub fn pin_bottom(_window: &tauri::WebviewWindow) -> Result<(), String> {
    Err("PinBottom on Windows not supported".to_string())
}

#[allow(dead_code)]
pub fn unpin_bottom(_window: &tauri::WebviewWindow) -> Result<(), String> {
    Ok(())
}

#[allow(dead_code)]
pub fn setup_hover_emitter(_window: &tauri::WebviewWindow) -> Result<(), String> {
    Ok(())
}