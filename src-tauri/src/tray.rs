// 系统托盘 —— v0.1 stub
//
// v0.1 只做"显示/隐藏浮窗 + 退出"，图标先空（v0.2 加"任务总数 badge"）
// Musage 的托盘是动态绘制（image + imageproc + ab_glyph），Usticky v0.1
// 先用静态 placeholder 图标，v0.2 再做动态 badge。

use anyhow::Result;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};

pub fn build_tray(app: &AppHandle) -> Result<()> {
    // 文案走 rust_i18n（跟前端 en.json / zh-CN.json 同步），
    // 菜单 key 在 locale 文件里 tray.show / tray.hide / tray.quit。
    let show = MenuItem::with_id(
        app,
        "show",
        rust_i18n::t!("tray.show"),
        true,
        None::<&str>,
    )?;
    let hide = MenuItem::with_id(
        app,
        "hide",
        rust_i18n::t!("tray.hide"),
        true,
        None::<&str>,
    )?;
    let quit = MenuItem::with_id(
        app,
        "quit",
        rust_i18n::t!("tray.quit"),
        true,
        None::<&str>,
    )?;
    let menu = Menu::with_items(app, &[&show, &hide, &quit])?;

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