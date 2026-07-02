// Linux stub

#[allow(dead_code)]
pub fn pin_bottom(_window: &tauri::WebviewWindow) -> Result<(), String> {
    Err("PinBottom on Linux not supported".to_string())
}

#[allow(dead_code)]
pub fn unpin_bottom(_window: &tauri::WebviewWindow) -> Result<(), String> {
    Ok(())
}

#[allow(dead_code)]
pub fn setup_hover_emitter(_window: &tauri::WebviewWindow) -> Result<(), String> {
    Ok(())
}