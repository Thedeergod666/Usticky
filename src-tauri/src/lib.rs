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
use tauri::{Emitter, Listener, Manager};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

// rust_i18n crate 级初始化 —— 让 commands / tray 等模块都能直接 t!("xxx")。
// 文件放在 src-tauri/locales/{en,zh-CN}.json，跟前端 en.json / zh-CN.json
// 解耦（rust_i18n 不支持嵌套 dotted key，跟前端 dict 分开维护）。
rust_i18n::i18n!("locales");

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
        .plugin(tauri_plugin_opener::init())
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
                // clamp 到主显示器范围内 —— 上次插着副屏、副屏拔了的话，
                // 直接 set_position 会把窗口扔到屏幕外。
                let mon = app.primary_monitor().ok().flatten();
                let (mx, my, mw, mh) = mon
                    .map(|m| {
                        let s = m.size();
                        let p = m.position();
                        (p.x, p.y, s.width as i32, s.height as i32)
                    })
                    .unwrap_or((0, 0, 1920, 1080));
                if let (Some(x), Some(y)) = (geom.x, geom.y) {
                    let cx = x.clamp(mx.saturating_sub(50), mx + mw - 50);
                    let cy = y.clamp(my.saturating_sub(10), my + mh - 10);
                    let _ = window.set_position(tauri::PhysicalPosition::new(cx, cy));
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

            // 6. 注册浮窗位置/尺寸持久化（Musage 经验：spawn 异步写，不阻塞 UI 线程，
            //    **关键**：spawn 里先 write guard 内 update 内存态 → drop guard →
            //    再 persist 磁盘。write guard 跨 I/O 会让 IPC add_todo 排队。
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
                            {
                                let mut s = store.write().await;
                                s.update_window_pos(Some(x), Some(y));
                            } // drop guard 在 await 之间，避免与 add_todo 排队
                            if let Err(e) = store.read().await.persist(&app) {
                                tracing::error!("persist window pos failed: {}", e);
                                let _ = app.emit("usticky://persist-failed", e.to_string());
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
                            {
                                let mut s = store.write().await;
                                s.update_window_size(Some(w), Some(h));
                            }
                            if let Err(e) = store.read().await.persist(&app) {
                                tracing::error!("persist window size failed: {}", e);
                                let _ = app.emit("usticky://persist-failed", e.to_string());
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

            // 7. locale 切换链路：tray 菜单 + settings 窗口 title 同步重建
            //    单一来源 = 后端 locales/{en,zh-CN}.json，前端只镜像一份。
            //    tray 重建走 tray::rebuild_tray（内部派发到 main thread 避免
            //    NSStatusBar 跨线程 SIGTRAP）。settings 窗口可能没开，需判 None。
            let app_for_locale = app.handle().clone();
            app.listen("usticky://locale-changed", move |_| {
                if let Err(e) = tray::rebuild_tray(&app_for_locale) {
                    tracing::warn!(error = %e, "rebuild_tray 失败");
                }
                if let Some(w) = app_for_locale.get_webview_window("settings") {
                    let title = rust_i18n::t!("window.settings").to_string();
                    if let Err(e) = w.set_title(&title) {
                        tracing::warn!(error = %e, "set settings window title 失败");
                    }
                }
            });

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
            commands::open_settings_window,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Usticky");
}