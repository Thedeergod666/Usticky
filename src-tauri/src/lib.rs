// Usticky 后端入口
//
// 关键设计（沿用 Musage v0.2 经验，详见 ~/Project/Usticky/AGENTS.md）：
//   - crate-type = ["staticlib", "rlib"] 绕过 MinGW ld 16-bit ordinal 溢出
//   - tokio::sync::RwLock<Store> 持有内存态，IPC 走 &State<...>
//   - WindowEvent::Moved/Resized → spawn 异步任务持久化（不阻塞 UI 线程）
//   - 单文件 JSON 原子写：tmp → rename + Unix 0600 + parse 失败 backup .bak.<ts>
//   - 跨平台 pin mode 三档：pin_top / pin_bottom / normal
//     （macOS: NSWindow.setLevel; Win: HWND_TOPMOST/BOTTOM; Linux: no-op）
//   - hover emitter 50ms tick 永远运行（驱动 CSS glass 效果），
//     PinBottom 模式额外切 NSWindow level / Win z-order
//
// 不沿用 Musage 的：
//   - 11 provider / QuotaSource trait
//   - poller / backoff
//   - tray 动态进度条（Usticky tray 是"任务总数 badge"，v0.1 stub）
//   - PinBottom hover emitter 在 Musage 是 v0.2 才加的，Usticky v0.1 直接搬

use std::sync::Arc;
use tauri::{Emitter, Manager};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

mod commands;
mod platform;
mod todo;
mod tray;

use todo::{PinMode, Store};

pub type SharedStore = Arc<tokio::sync::RwLock<Store>>;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .compact()
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_autostart::Builder::new()
            .args(vec!["--autostart"])
            .build())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            // 1. 加载或初始化 todo store
            let store = Store::load_or_init(app.handle())
                .expect("failed to init todo store");
            let store: SharedStore = Arc::new(tokio::sync::RwLock::new(store));

            // 2. 启动时恢复浮窗位置/尺寸（Musage 经验）
            if let Some(window) = app.get_webview_window("floating") {
                let geom = {
                    let s = store.blocking_read();
                    s.last_window_geom().clone()
                };
                if let (Some(x), Some(y)) = (geom.x, geom.y) {
                    let _ = window.set_position(tauri::PhysicalPosition::new(x, y));
                }
                if let (Some(w), Some(h)) = (geom.width, geom.height) {
                    if w > 0 && h > 0 {
                        let _ = window.set_size(tauri::PhysicalSize::new(w, h));
                    }
                }
            }

            app.manage(store.clone());

            // 3. 注册全局快捷键：CmdOrCtrl+Shift+Space → emit quick-add
            let shortcut = Shortcut::new(
                Some(Modifiers::CONTROL | Modifiers::SHIFT),
                Code::Space,
            );
            let app_handle = app.handle().clone();
            app.global_shortcut().on_shortcut(shortcut, move |_app, _shortcut, event| {
                if event.state() == ShortcutState::Pressed {
                    if let Some(w) = app_handle.get_webview_window("floating") {
                        let _ = w.show();
                        let _ = w.set_focus();
                        let _ = app_handle.emit("usticky://quick-add", ());
                    }
                }
            })?;

            // 4. 系统托盘（v0.1 stub：显示/隐藏/退出）
            tray::build_tray(app.handle())?;

            // 5. 启动 hover emitter + 应用上次持久化的 pin mode
            //    （Musage 经验：tracker 始终跑，不分 pin mode；
            //      LEVEL_SWITCHING_ACTIVE 在 PinBottom 模式才翻 true）
            let initial_pin_mode = store.blocking_read().pin_mode();
            match initial_pin_mode {
                PinMode::PinTop => platform::set_window_pin_top(app.handle()),
                PinMode::PinBottom => platform::set_window_pin_bottom(app.handle()),
                PinMode::Normal => platform::set_window_normal(app.handle()),
            }
            // PinTop / Normal 模式时 start_hover_emitter 不会被内部调，
            // 但 hover 事件 emit 仍要工作（驱动 CSS glass 效果），
            // 所以无条件下调一次启动 tracker。
            platform::start_hover_emitter(app.handle().clone());

            // 5. 注册浮窗位置/尺寸持久化（Musage 经验：spawn 异步写，不阻塞 UI 线程）
            if let Some(window) = app.get_webview_window("floating") {
                let store_for_geom = store.clone();
                let app_handle_geom = app.handle().clone();
                let window_for_close = window.clone();
                window.on_window_event(move |event| match event {
                    tauri::WindowEvent::Moved(pos) => {
                        let store = store_for_geom.clone();
                        let app = app_handle_geom.clone();
                        let (x, y) = (pos.x, pos.y);
                        tauri::async_runtime::spawn(async move {
                            let mut s = store.write().await;
                            s.update_window_pos(Some(x), Some(y));
                            if let Err(e) = s.persist(&app) {
                                tracing::error!("persist window pos failed: {}", e);
                            }
                        });
                    }
                    tauri::WindowEvent::Resized(size) => {
                        // 过滤掉 (0, 0) —— 启动前 fire 的占位 resize
                        if size.width <= 0 || size.height <= 0 { return; }
                        let store = store_for_geom.clone();
                        let app = app_handle_geom.clone();
                        let (w, h) = (size.width, size.height);
                        tauri::async_runtime::spawn(async move {
                            let mut s = store.write().await;
                            s.update_window_size(Some(w), Some(h));
                            if let Err(e) = s.persist(&app) {
                                tracing::error!("persist window size failed: {}", e);
                            }
                        });
                    }
                    tauri::WindowEvent::CloseRequested { api, .. } => {
                        // 点 X 不退出 app，浮窗进 hide 状态（Musage 经验）
                        api.prevent_close();
                        let _ = window_for_close.hide();
                    }
                    _ => {}
                });
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_todos,
            commands::add_todo,
            commands::update_todo,
            commands::delete_todo,
            commands::reorder_todos,
            commands::resize_floating_window,
            commands::reset_floating_window,
            commands::hide_floating_window,
            commands::show_floating_window,
            commands::get_app_locale,
            commands::set_app_locale,
            commands::get_pin_mode,
            commands::set_pin_mode,
            commands::set_floating_hover_raise,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Usticky");
}