// IPC commands —— 暴露给前端的 #[tauri::command]
//
// 设计：commands 都很瘦，只做 (1) 拿 store 引用 (2) 调 store 方法 (3) emit
// todos-changed 事件。所有业务逻辑在 Store 里。
//
// DTO 全部 #[serde(rename_all = "camelCase")] —— Tauri 2 对 struct 字段
// 也走 camelCase 转换（Musage PR 1b 实测坑）。
use tauri::{AppHandle, Emitter, Manager, State};

use crate::todo::{Todo, TodoStatus, TodoSnapshot};
use crate::SharedStore;

fn emit_todos_changed(app: &AppHandle, snap: &TodoSnapshot) {
    let _ = app.emit("usticky://todos-changed", snap);
}

async fn persist_and_emit(app: &AppHandle, store: &SharedStore) -> TodoSnapshot {
    let snap = {
        let s = store.read().await;
        s.snapshot()
    };
    if let Err(e) = store.read().await.persist(app) {
        tracing::error!("persist failed: {}", e);
    }
    emit_todos_changed(app, &snap);
    snap
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
        return Err("empty title".into());
    }
    if trimmed.chars().count() > 280 {
        return Err("title too long".into());
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
    let status_enum = status.map(|s| match s.as_str() {
        "done" => TodoStatus::Done,
        _ => TodoStatus::Pending,
    });
    let updated = {
        let mut s = store.write().await;
        s.update(&id, title, status_enum)
            .ok_or_else(|| "not found".to_string())?
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
        s.delete(&id).ok_or_else(|| "not found".to_string())?
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
        .ok_or_else(|| "no primary monitor".to_string())?;
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
        persist_and_emit(&app, &store).await;
    }
    Ok(())
}

#[tauri::command]
pub async fn resize_floating_window(app: AppHandle, height: f64) -> Result<(), String> {
    if let Some(w) = app.get_webview_window("floating") {
        let cur = w.outer_size().map_err(|e| e.to_string())?;
        w.set_size(tauri::PhysicalSize::new(cur.width, height as u32))
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