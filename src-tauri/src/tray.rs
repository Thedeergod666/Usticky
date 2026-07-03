// 系统托盘 —— v0.1
//
// v0.1 静态图标（tray-base.png，scripts/generate_icons.py 生成 U 字母版）。
// v0.2 候选：任务总数 badge（参考 Musage 的动态绘制）。
//
// 菜单 4 项：Show / Hide / Settings... / Quit。
// locale 切换时 lib.rs 的 `usticky://locale-changed` listener 调 rebuild_tray，
// 重新构造菜单（label 走 t!() 拿当前 locale 文案）+ set_menu 替换（不闪烁）。
//
// rebuild_tray 通过 `app.run_on_main_thread` 派发到 main thread：
// `tray_by_id` 返回的 owned `TrayIcon` 出 scope 会 drop，跨线程 drop
// 触发 NSStatusBar `assertBarrierOnQueue` SIGTRAP（Musage 2026-06-18 踩过）。
// Usticky 没有 poller 高频写 tray，所以不需要 mpsc channel，单次派发足够。

use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, Runtime,
};

use crate::commands;

/// 构造 tray 菜单（独立成函数，方便 [`rebuild_tray`] 在 locale 切换时复用）。
///
/// 4 项 menu label 全部走 t!()（i18n）。切换语言时 [`rebuild_tray`]
/// 重新构造菜单 + set_menu 替换。
fn build_tray_menu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Menu<R>> {
    let show_i = MenuItem::with_id(
        app,
        "show",
        rust_i18n::t!("tray.show").to_string(),
        true,
        None::<&str>,
    )?;
    let hide_i = MenuItem::with_id(
        app,
        "hide",
        rust_i18n::t!("tray.hide").to_string(),
        true,
        None::<&str>,
    )?;
    let settings_i = MenuItem::with_id(
        app,
        "settings",
        rust_i18n::t!("tray.settings").to_string(),
        true,
        None::<&str>,
    )?;
    let sep_i = PredefinedMenuItem::separator(app)?;
    let quit_i = MenuItem::with_id(
        app,
        "quit",
        rust_i18n::t!("tray.quit").to_string(),
        true,
        None::<&str>,
    )?;
    Menu::with_items(app, &[&show_i, &hide_i, &sep_i, &settings_i, &quit_i])
}

pub fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let menu = build_tray_menu(app)?;

    let _tray = TrayIconBuilder::with_id("main-tray")
        .menu(&menu)
        // 左键单击 = 切换浮窗显隐（AGENTS.md 第 12 条）。右键才弹菜单。
        .show_menu_on_left_click(false)
        .icon(app.default_window_icon().cloned().unwrap_or_else(|| {
            // 兜底：理论上 default_window_icon 一定有；fallback 用 app 内置
            tauri::image::Image::from_bytes(include_bytes!("../icons/tray-base.png"))
                .expect("tray icon")
        }))
        .on_menu_event(move |app, event| match event.id().as_ref() {
            "show" => {
                if let Some(w) = app.get_webview_window("floating") {
                    let _ = w.show();
                    let _ = w.set_focus();
                }
            }
            "hide" => {
                if let Some(w) = app.get_webview_window("floating") {
                    let _ = w.hide();
                }
            }
            "settings" => {
                let app2 = app.clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(e) = commands::open_settings_window(app2).await {
                        tracing::warn!(error = %e, "打开设置失败");
                    }
                });
            }
            "quit" => {
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            // 左键单击 = 切换浮窗显隐
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let app = tray.app_handle();
                if let Some(w) = app.get_webview_window("floating") {
                    let is_visible = w.is_visible().unwrap_or(false);
                    if is_visible {
                        let _ = w.hide();
                    } else {
                        let _ = w.show();
                        let _ = w.set_focus();
                    }
                }
            }
        })
        .build(app)?;

    Ok(())
}

/// locale 切换时重新构造菜单并 `set_menu()` 替换（不是 remove+add，
/// 避免 tray 短暂消失闪烁）。
///
/// 调用时机：[`crate::lib::run`] setup 里的 `usticky://locale-changed` 监听器。
/// listener callback 可能不在 main thread，所以通过 `run_on_main_thread`
/// 派发到 main thread 上做 `tray_by_id` + `set_menu` + drop（drop 在 main
/// thread 跑才不会触发 NSStatusBar SIGTRAP）。
pub fn rebuild_tray(app: &AppHandle) -> tauri::Result<()> {
    let app2 = app.clone();
    app.run_on_main_thread(move || {
        let Some(tray) = app2.tray_by_id("main-tray") else {
            tracing::warn!("rebuild_tray: tray_by_id 返 None（tray 还没建好？）");
            return;
        };
        match build_tray_menu(&app2) {
            Ok(menu) => {
                if let Err(e) = tray.set_menu(Some(menu)) {
                    tracing::warn!(error = %e, "rebuild_tray: set_menu 失败");
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "rebuild_tray: build_tray_menu 失败");
            }
        }
        // tray 在 main thread 上自然 drop，安全
    })?;
    Ok(())
}
