// IPC commands —— 暴露给前端的 #[tauri::command]
//
// 设计：commands 都很瘦，只做 (1) 拿 store 引用 (2) 调 store 方法 (3) emit
// todos-changed 事件。所有业务逻辑在 Store 里。
//
// DTO 全部 #[serde(rename_all = "camelCase")] —— Tauri 2 对 struct 字段
// 也走 camelCase 转换（Musage PR 1b 实测坑）。
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder};

use crate::todo::{PinMode, Todo, TodoStatus, TodoSnapshot};
use crate::SharedStore;

/// 把 pin mode 应用到窗口（跨平台，platform/mod.rs 统一导出）。
pub fn apply_pin_mode_to_window(app: &AppHandle, mode: PinMode) {
    match mode {
        PinMode::PinTop => crate::platform::set_window_pin_top(app),
        PinMode::PinBottom => crate::platform::set_window_pin_bottom(app),
        PinMode::Normal => crate::platform::set_window_normal(app),
    }
}

fn emit_todos_changed(app: &AppHandle, snap: &TodoSnapshot) {
    let _ = app.emit("usticky://todos-changed", snap);
}

/// 落盘 + emit todos-changed。
///
/// persist 失败时（磁盘满 / 权限被剥 / 临时目录异常）不再静默吞掉，而是
/// emit `usticky://persist-failed` 让前端 mini-flash 提示用户 —— 否则前端
/// invoke 拿到 Ok 后以为写成功了，下次启动数据全没。
async fn persist_and_emit(app: &AppHandle, store: &SharedStore) -> TodoSnapshot {
    let snap = {
        let s = store.read().await;
        s.snapshot()
    };
    if let Err(e) = store.read().await.persist(app) {
        tracing::error!("persist failed: {}", e);
        let _ = app.emit("usticky://persist-failed", e.to_string());
    }
    emit_todos_changed(app, &snap);
    snap
}

/// 状态字符串 → enum。非法值直接报错，让前端知道走错了路径。
fn parse_status(s: &str) -> Result<TodoStatus, String> {
    match s {
        "pending" => Ok(TodoStatus::Pending),
        "done" => Ok(TodoStatus::Done),
        other => Err(format!("invalid status: {}", other)),
    }
}

// ── CRUD ──

#[tauri::command]
pub async fn get_todos(store: State<'_, SharedStore>) -> Result<TodoSnapshot, String> {
    Ok(store.read().await.snapshot())
}

#[tauri::command]
pub async fn add_todo(
    app: AppHandle,
    store: State<'_, SharedStore>,
    title: String,
) -> Result<Todo, String> {
    let trimmed = title.trim().to_string();
    if trimmed.is_empty() {
        return Err(rust_i18n::t!("commands.error.empty_title").into());
    }
    if trimmed.chars().count() > 280 {
        return Err(rust_i18n::t!("commands.error.too_long").into());
    }
    let todo = {
        let mut s = store.write().await;
        s.add(trimmed)
    };
    persist_and_emit(&app, &store).await;
    Ok(todo)
}

#[tauri::command]
pub async fn update_todo(
    app: AppHandle,
    store: State<'_, SharedStore>,
    id: String,
    title: Option<String>,
    status: Option<String>,
) -> Result<Todo, String> {
    let status_enum = match status {
        Some(s) => Some(parse_status(&s)?),
        None => None,
    };
    let updated = {
        let mut s = store.write().await;
        s.update(&id, title, status_enum)
            .ok_or_else(|| rust_i18n::t!("commands.error.not_found").to_string())?
    };
    persist_and_emit(&app, &store).await;
    Ok(updated)
}

#[tauri::command]
pub async fn delete_todo(
    app: AppHandle,
    store: State<'_, SharedStore>,
    id: String,
) -> Result<Todo, String> {
    let deleted = {
        let mut s = store.write().await;
        s.delete(&id)
            .ok_or_else(|| rust_i18n::t!("commands.error.not_found").to_string())?
    };
    persist_and_emit(&app, &store).await;
    Ok(deleted)
}

#[tauri::command]
pub async fn reorder_todos(
    app: AppHandle,
    store: State<'_, SharedStore>,
    ids: Vec<String>,
) -> Result<(), String> {
    {
        let mut s = store.write().await;
        s.reorder(&ids);
    }
    persist_and_emit(&app, &store).await;
    Ok(())
}

// ── 浮窗控制 ──

#[tauri::command]
pub async fn show_floating_window(app: AppHandle) -> Result<(), String> {
    if let Some(w) = app.get_webview_window("floating") {
        w.show().map_err(|e| e.to_string())?;
        w.set_focus().map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub async fn hide_floating_window(app: AppHandle) -> Result<(), String> {
    if let Some(w) = app.get_webview_window("floating") {
        w.hide().map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub async fn reset_floating_window(
    app: AppHandle,
    store: State<'_, SharedStore>,
) -> Result<(), String> {
    let monitor = app.primary_monitor().map_err(|e| e.to_string())?
        .ok_or_else(|| rust_i18n::t!("commands.error.no_primary_monitor").to_string())?;
    let mon_size = monitor.size();
    let mon_pos = monitor.position();
    if let Some(w) = app.get_webview_window("floating") {
        let win_size = w.outer_size().map_err(|e| e.to_string())?;
        let x = mon_pos.x + ((mon_size.width as i32 - win_size.width as i32) / 2);
        let y = mon_pos.y + ((mon_size.height as i32 - win_size.height as i32) / 2);
        w.set_position(tauri::PhysicalPosition::new(x, y))
            .map_err(|e| e.to_string())?;
        {
            let mut s = store.write().await;
            s.update_window_pos(Some(x), Some(y));
        }
        // 落盘（窗口几何 + todos 一起）但不 emit todos-changed —— 复位位置
        // 跟 todo 列表无关，前端不要白白 render 一遍。
        if let Err(e) = store.read().await.persist(&app) {
            tracing::error!("persist failed: {}", e);
            let _ = app.emit("usticky://persist-failed", e.to_string());
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn resize_floating_window(app: AppHandle, height: f64) -> Result<(), String> {
    if let Some(w) = app.get_webview_window("floating") {
        let cur = w.outer_size().map_err(|e| e.to_string())?;
        // 前端传的 height 是 CSS 像素（logical），PhysicalSize 期望物理像素。
        // 不转 dpr 的话 Retina（scale=2）上窗口实际高度只有预期的一半，
        // 视觉上就是"自适应不工作"。
        let scale = w.scale_factor().unwrap_or(1.0);
        let new_h_physical = (height * scale).round() as u32;
        // width 沿用 outer_size 返回的物理像素，不改动用户拖拽的宽度
        w.set_size(tauri::PhysicalSize::new(cur.width, new_h_physical))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

// ── i18n ──

#[tauri::command]
pub fn get_app_locale() -> String {
    rust_i18n::locale().to_string()
}

#[tauri::command]
pub fn set_app_locale(app: AppHandle, locale: String) -> Result<(), String> {
    rust_i18n::set_locale(&locale);
    let _ = app.emit("usticky://locale-changed", locale);
    Ok(())
}

// ── Pin mode ──

#[tauri::command]
pub async fn get_pin_mode(store: State<'_, SharedStore>) -> Result<String, String> {
    let s = store.read().await;
    Ok(match s.pin_mode() {
        PinMode::PinTop => "pin_top".into(),
        PinMode::PinBottom => "pin_bottom".into(),
        PinMode::Normal => "normal".into(),
    })
}

#[tauri::command]
pub async fn set_pin_mode(
    app: AppHandle,
    store: State<'_, SharedStore>,
    mode: String,
) -> Result<(), String> {
    set_pin_mode_core(&app, store.inner(), &mode).await
}

/// pin mode 切换的核心逻辑（command 和 tray menu handler 共用）。
///
/// 走手写 persist 路径 + emit `usticky://pin-mode-changed`（不走 persist_and_emit，
/// 因为 pin mode 改了跟 todo 列表无关，前端不该 render todos）。
pub async fn set_pin_mode_core(
    app: &AppHandle,
    store: &SharedStore,
    mode: &str,
) -> Result<(), String> {
    let parsed = PinMode::from_str_opt(mode)
        .ok_or_else(|| format!("invalid pin mode: {}", mode))?;
    apply_pin_mode_to_window(app, parsed);
    {
        let mut s = store.write().await;
        s.set_pin_mode(parsed);
    }
    if let Err(e) = store.read().await.persist(app) {
        tracing::error!("persist failed: {}", e);
        let _ = app.emit("usticky://persist-failed", e.to_string());
    }
    let _ = app.emit("usticky://pin-mode-changed", mode);
    Ok(())
}

#[tauri::command]
pub async fn set_floating_hover_raise(
    app: AppHandle,
    store: State<'_, SharedStore>,
    hovering: bool,
) -> Result<(), String> {
    let mode = store.read().await.pin_mode();
    if mode != PinMode::PinBottom {
        return Ok(());
    }
    crate::platform::set_window_hover_raise(&app, hovering);
    Ok(())
}

// ── 设置窗口 ──

/// 打开设置窗口（已在则 focus，未建则动态创建）。
///
/// 不在 tauri.conf.json 的 windows 数组里声明 —— 用户只在点"设置..."时
/// 才需要这个窗口，常驻会拖慢启动 + 占内存。动态创建 + 关闭时 destroy
/// 是 Musage 同款路径。
///
/// 窗口属性沿用 Musage：常规带 decorations 窗口、可调整大小、居中、
/// 适中的初始尺寸（窄到 ~620x520，能放下单页设置内容）。
#[tauri::command]
pub async fn open_settings_window(app: AppHandle) -> Result<(), String> {
    if let Some(w) = app.get_webview_window("settings") {
        // 已开 —— 直接 focus，不重复创建（避免多实例 + 状态分裂）
        w.show().map_err(|e| e.to_string())?;
        w.set_focus().map_err(|e| e.to_string())?;
        return Ok(());
    }
    let title = rust_i18n::t!("window.settings").to_string();
    let _win = WebviewWindowBuilder::new(&app, "settings", WebviewUrl::App("settings.html".into()))
        .title(title)
        .inner_size(620.0, 520.0)
        .min_inner_size(480.0, 360.0)
        .resizable(true)
        .decorations(true)
        .transparent(false)
        .shadow(true)
        .visible(true)
        .center()
        .build()
        .map_err(|e| format!("create settings window: {e}"))?;
    Ok(())
}