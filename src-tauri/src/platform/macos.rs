// macOS 私有 API —— v0.1 stub
//
// v0.1 不做 PinBottom。浮窗 alwaysOnTop: true 已经够了。
// v2+ 想要"PinBottom 模式"时，参考 ~/Project/Musage/src-tauri/src/platform/macos.rs
// 的完整实现：NSWindow.setLevel(-1) + NSEvent.mouseLocation 全局轮询 + WKWebView
// 非 key window 不分发 mouseMoved 的绕过。代码约 200 行，依赖 objc2 全家桶。
//
// 留空 stub 是为了不让 build.rs 在 macOS 上找不到模块。

#[allow(dead_code)]
pub fn pin_bottom(_window: &tauri::WebviewWindow) -> Result<(), String> {
    Err("PinBottom 暂未实现 (Usticky v2+ 计划)".to_string())
}

#[allow(dead_code)]
pub fn unpin_bottom(_window: &tauri::WebviewWindow) -> Result<(), String> {
    Ok(())
}

#[allow(dead_code)]
pub fn setup_hover_emitter(_window: &tauri::WebviewWindow) -> Result<(), String> {
    Ok(())
}