// 系统托盘 —— v0.1.2
//
// v0.1.2：把 "Settings..." 顶层项改为 "Settings" 子菜单，内含 pin mode
// 三档（Top/Bottom/Normal，CheckMenuItem 带 checkmark）+ "Open Settings Panel..."
// 打开完整设置窗口。pin mode 从任何地方改（浮窗 foot / 设置面板 / tray 子菜单）
// 都会 emit `usticky://pin-mode-changed` → lib.rs listener 调 rebuild_tray 刷新
// checkmark。
//
// v0.1 静态图标（tray-base.png，scripts/generate_icons.py 生成 U 字母版）。
// v0.2 候选：任务总数 badge（参考 Musage 的动态绘制）。
//
// 菜单结构：
//   Toggle floating window
//   ---
//   [Settings ▸]
//     Top        ✓
//     Bottom     ✓
//     Normal     ✓
//     ---
//     Open Settings Panel...
//   ---
//   Quit Usticky
//
// locale 切换 / pin mode 切换时 lib.rs 的 listener 调 rebuild_tray，
// 重新构造菜单（label 走 t!() 拿当前 locale 文案 + checkmark 走当前 pin mode）
// + set_menu 替换（不闪烁）。
//
// rebuild_tray 通过 `app.run_on_main_thread` 派发到 main thread：
// `tray_by_id` 返回的 owned `TrayIcon` 出 scope 会 drop，跨线程 drop
// 触发 NSStatusBar `assertBarrierOnQueue` SIGTRAP（Musage 2026-06-18 踩过）。
// Usticky 没有 poller 高频写 tray，所以不需要 mpsc channel，单次派发足够。

use tauri::{
    menu::{CheckMenuItem, IsMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, Runtime,
};

use crate::commands;
use crate::todo::PinMode;
use crate::SharedStore;

/// 读 store 当前 pin mode（blocking_read 在 sync 上下文安全 —— 跟 lib.rs setup 同款）。
fn current_pin_mode<R: Runtime>(app: &AppHandle<R>) -> PinMode {
    match app.try_state::<SharedStore>() {
        Some(store) => store.blocking_read().pin_mode(),
        None => PinMode::default(),
    }
}

/// 读 store 当前 quick-add 快捷键（accelerator 字符串）。
fn current_quick_add_shortcut<R: Runtime>(app: &AppHandle<R>) -> String {
    match app.try_state::<SharedStore>() {
        Some(store) => store.blocking_read().quick_add_shortcut(),
        None => crate::todo::default_quick_add_shortcut(),
    }
}

/// 把 accelerator 字符串（如 `"Cmd+Shift+Space"`）转成更友好的展示形式
/// （如 macOS 上 `"⌘⇧Space"`、其他平台 `"Ctrl+Shift+Space"`）。
/// 仅用于 tray menu label / 设置面板显示，不影响实际注册逻辑。
fn format_shortcut_for_display(s: &str) -> String {
    #[cfg(target_os = "macos")]
    {
        let mut out = String::new();
        for part in s.split('+') {
            let p = part.trim();
            match p.to_lowercase().as_str() {
                "cmd" | "command" | "super" | "meta" | "cmdorctrl" | "cmdorcontrol"
                    | "commandorctrl" | "commandorcontrol" => out.push('⌘'),
                "ctrl" | "control" => out.push('⌃'),
                "shift" => out.push('⇧'),
                "alt" | "option" | "opt" => out.push('⌥'),
                _ => out.push_str(p),
            }
        }
        out
    }
    #[cfg(not(target_os = "macos"))]
    {
        s.to_string()
    }
}

/// 构造 tray 菜单（独立成函数，方便 [`rebuild_tray`] 在 locale / pin mode 切换时复用）。
///
/// 所有 label 走 t!()（i18n）。pin mode 项用 CheckMenuItem，checkmark 跟当前
/// 持久化的 pin mode 对齐。
fn build_tray_menu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Menu<R>> {
    let toggle_i = MenuItem::with_id(
        app,
        "toggle",
        rust_i18n::t!("tray.toggle").to_string(),
        true,
        None::<&str>,
    )?;

    // ── Settings 子菜单：pin mode 三档 + 打开设置面板 ──
    let cur_pin = current_pin_mode(app);
    let pin_top_i = CheckMenuItem::with_id(
        app,
        "pin_top",
        rust_i18n::t!("tray.pin.top").to_string(),
        true,
        cur_pin == PinMode::PinTop,
        None::<&str>,
    )?;
    let pin_bottom_i = CheckMenuItem::with_id(
        app,
        "pin_bottom",
        rust_i18n::t!("tray.pin.bottom").to_string(),
        true,
        cur_pin == PinMode::PinBottom,
        None::<&str>,
    )?;
    let pin_normal_i = CheckMenuItem::with_id(
        app,
        "normal",
        rust_i18n::t!("tray.pin.normal").to_string(),
        true,
        cur_pin == PinMode::Normal,
        None::<&str>,
    )?;
    let sep_pin = PredefinedMenuItem::separator(app)?;
    let open_settings_i = MenuItem::with_id(
        app,
        "settings",
        rust_i18n::t!("tray.open_settings").to_string(),
        true,
        None::<&str>,
    )?;
    let sep_settings = PredefinedMenuItem::separator(app)?;
    let cur_sc = current_quick_add_shortcut(app);
    let sc_label = format!("{} ({})",
        rust_i18n::t!("tray.quick_add_change").to_string(),
        format_shortcut_for_display(&cur_sc));
    let change_shortcut_i = MenuItem::with_id(
        app,
        "change_shortcut",
        sc_label,
        true,
        None::<&str>,
    )?;
    let settings_sub = Submenu::with_items(
        app,
        rust_i18n::t!("tray.settings").to_string(),
        true,
        &[
            &pin_top_i as &dyn IsMenuItem<R>,
            &pin_bottom_i as &dyn IsMenuItem<R>,
            &pin_normal_i as &dyn IsMenuItem<R>,
            &sep_pin as &dyn IsMenuItem<R>,
            &open_settings_i as &dyn IsMenuItem<R>,
            &sep_settings as &dyn IsMenuItem<R>,
            &change_shortcut_i as &dyn IsMenuItem<R>,
        ],
    )?;

    let sep_i = PredefinedMenuItem::separator(app)?;
    let quit_i = MenuItem::with_id(
        app,
        "quit",
        rust_i18n::t!("tray.quit").to_string(),
        true,
        None::<&str>,
    )?;
    Menu::with_items(app, &[&toggle_i, &sep_i, &settings_sub, &quit_i])
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
            "toggle" => {
                // 走统一 show / hide_dismiss 路径 —— 保持 quick-add 状态一致性。
                //
                // **关键**：show 分支用 `quick_show_floating_window`（raise + show + focus），
                // 不是裸的 `w.show() + w.set_focus()`。PinBottom 默认 mode 下裸 show
                // 会让窗口停在 level=-1，被任何 app 盖住 → 用户"看不到浮窗"。
                // raise 完再 show 视觉一致，dismiss 时按 pin mode 还原。
                if let Some(w) = app.get_webview_window("floating") {
                    let is_visible = w.is_visible().unwrap_or(false);
                    if is_visible {
                        if let Some(store) = app.try_state::<crate::SharedStore>() {
                            crate::hide_dismiss_floating_window(app, store.inner());
                        } else {
                            let _ = w.hide();
                        }
                    } else {
                        // **P1-5 fix**：tray 左键单击的 show 分支走"普通 show"
                        // 路径，不激活 QUICK_ADD_ACTIVE。否则用户托盘点开浮窗
                        // → 切别 app → 被 blur_dismiss 还原 level 盖住。
                        crate::show_floating_window_normal(app);
                    }
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
            "change_shortcut" => {
                // 打开设置面板 —— 快捷键的录入 UI 在设置面板里。
                // 直接在 tray 菜单里做录入（NSMenu 不支持捕获 keydown）。
                let app2 = app.clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(e) = commands::open_settings_window(app2).await {
                        tracing::warn!(error = %e, "打开设置失败");
                    }
                });
            }
            "pin_top" | "pin_bottom" | "normal" => {
                let app2 = app.clone();
                let mode = event.id().as_ref().to_string();
                tauri::async_runtime::spawn(async move {
                    let store = app2.state::<SharedStore>().inner().clone();
                    if let Err(e) = commands::set_pin_mode_core(&app2, &store, &mode).await {
                        tracing::warn!(error = %e, "tray set_pin_mode 失败");
                    }
                });
            }
            "quit" => {
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            // 左键单击 = 切换浮窗显隐。show 分支用 quick_show_floating_window
            // （raise + show + focus），见上 "toggle" 注释 —— PinBottom 默认 mode
            // 下裸 show 会停在 level=-1 被其它 app 盖住，看不到。
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
                        if let Some(store) = app.try_state::<crate::SharedStore>() {
                            crate::hide_dismiss_floating_window(app, store.inner());
                        } else {
                            let _ = w.hide();
                        }
                    } else if let Some(store) = app.try_state::<crate::SharedStore>() {
                        crate::quick_show_floating_window(app, store.inner());
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

/// locale / pin mode 切换时重新构造菜单并 `set_menu()` 替换（不是 remove+add，
/// 避免 tray 短暂消失闪烁）。
///
/// 调用时机：[`crate::lib::run`] setup 里的 `usticky://locale-changed` 和
/// `usticky://pin-mode-changed` listener。
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
