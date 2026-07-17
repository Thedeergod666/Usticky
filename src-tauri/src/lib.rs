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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{Emitter, Listener, Manager};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

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

/// 是否处于 "quick-add 临时置顶" 状态。
///
/// true = 我们通过快捷键唤出了浮窗（已 raise 到 FLOATING + 焦点在输入框），
///        dismiss 时需要还原 level。
/// false = 浮窗处于其 pin mode 应有的 level（PinBottom/PinTop/Normal）。
///
/// 切换语义：
///   - 快捷键 + !active → save prev app + raise + show + focus + set true
///   - 快捷键 + active → toggle_dismiss（不隐藏，restore level + activate prev app + set false）
///   - 窗口失焦（Focused(false)）+ active → blur_dismiss（不隐藏，仅 restore level + set false；
///     不 activate prev app，因为用户已经点别处了，不该抢焦点回去）
///   - hide_floating_window 命令 / tray toggle hide / Esc → hide_dismiss（隐藏 + restore level
///     + activate prev app + set false）
///   - show_floating_window 命令 / tray toggle show → set false（清除残留状态）
static QUICK_ADD_ACTIVE: AtomicBool = AtomicBool::new(false);

/// 把 accelerator 字符串（如 `"Cmd+Shift+Space"`）解析成 [`Shortcut`]。
///
/// 直接走 `global-hotkey` 0.8 自带的字符串解析器（大小写不敏感、支持
/// `Cmd`/`Command`/`Super`/`CmdOrCtrl` 等多种别名 + 全部 `Code` 变体）。
/// **关键**：在 macOS 上 `Cmd`/`Super`/`CmdOrCtrl` → `Modifiers::SUPER`
/// （⌘ Command 键），`Ctrl`/`Control` → `Modifiers::CONTROL`（⌃ Control 键）。
/// 旧代码错用 `Modifiers::CONTROL` 当 ⌘ Cmd，注册的实际是 ⌃⇧Space。
fn parse_shortcut(s: &str) -> Result<Shortcut, String> {
    Shortcut::try_from(s).map_err(|e| format!("parse shortcut {:?}: {}", s, e))
}

/// 注册当前 store 里的 quick-add 快捷键。先 `unregister_all` 再注册，
/// 用于启动时 + `set_quick_add_shortcut` 切换时。
///
/// 失败不致命（极端情况下用户存了个 parse 不出来的字符串）—— log + emit
/// `usticky://persist-failed` 让前端提示，但 app 继续跑（快捷键只是不可用）。
fn register_quick_add_shortcut(app: &tauri::AppHandle, store: &SharedStore) {
    let accelerator = store.blocking_read().quick_add_shortcut();
    let gs = app.global_shortcut();
    // unregister_all 不会清掉其他 plugin 注册的快捷键（只清自己 register 的）
    let _ = gs.unregister_all();
    let parsed = match parse_shortcut(&accelerator) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("register_quick_add_shortcut: {}", e);
            let _ = app.emit("usticky://persist-failed", e);
            return;
        }
    };
    let app_handle = app.clone();
    let store_ref = store.clone();
    if let Err(e) = gs.on_shortcut(parsed, move |_app, _shortcut, event| {
        if event.state() != ShortcutState::Pressed {
            return;
        }
        // toggle 行为（不隐藏浮窗）：
        //   - !active → save prev app + raise + show + focus + set active
        //   - active → restore level + activate prev app + clear active（窗口保持可见）
        if QUICK_ADD_ACTIVE.load(Ordering::SeqCst) {
            // ── toggle dismiss 分支 ──
            // 不隐藏窗口，只还原 level + 切回原 app
            toggle_dismiss_floating_window(&app_handle, &store_ref);
            return;
        }
        // ── show 分支 ──
        quick_show_floating_window(&app_handle, &store_ref);
    }) {
        tracing::error!("on_shortcut failed: {}", e);
        let _ = app.emit("usticky://persist-failed", format!("register shortcut: {e}"));
    }
}

/// 内部 helper：清 QUICK_ADD_ACTIVE 状态 + 还原 level（不隐藏、不 activate prev app）。
/// 仅在 was_active=true 时做实际工作。
fn clear_quick_add_state(app: &tauri::AppHandle, store: &SharedStore) {
    let was_active = QUICK_ADD_ACTIVE.swap(false, Ordering::SeqCst);
    if was_active {
        let mode = store.blocking_read().pin_mode();
        platform::restore_level_after_quick_add(app, mode);
    }
}

/// "快速唤出"浮窗：save prev app + raise level + show + focus + 标记 active。
///
/// 被三个入口共用：
///   - 全局快捷键 Cmd+Shift+Space（raise_for_quick_add 之前已配 setHidesOnDeactivate(false)）
///   - tray 菜单 "Toggle floating window" 的 show 分支
///   - `show_floating_window` IPC 命令（设置窗口"打开浮窗"按钮等场景）
///
/// **为什么不是简单的 show() + set_focus()**：默认 PinBottom 模式 level=-1，
/// 浮窗在 -1 显示但被任何 app 窗口盖住 → 用户"看不到浮窗"。先 raise 到 FLOATING
/// 再 show，浮窗从 -1 升到 3 的过程不显示（hide 状态 → raise → show），视觉一致。
///
/// **跟 ESC / tray toggle hide 配合**：dismiss 时会读 QUICK_ADD_ACTIVE 决定是否
/// 还原 level。tray 走 hide_dismiss（hide + restore + activate），其他走 blur_dismiss
/// （仅 restore，不抢焦点）。
pub fn quick_show_floating_window(app: &tauri::AppHandle, _store: &SharedStore) {
    let Some(w) = app.get_webview_window("floating") else { return };
    // save prev app 只在快捷键路径有意图（tray 主动唤起不需要切回原 app，
    // activate_previous_app_after_quick_add 是 no-op 也不会报错 —— 但为了语义清晰，
    // 这里仍然 save：tray hide 走 hide_dismiss 会 activate prev app，跟快捷键一致）
    platform::save_previous_app_for_quick_add();
    platform::raise_for_quick_add(app);
    QUICK_ADD_ACTIVE.store(true, Ordering::SeqCst);
    let _ = w.show();
    let _ = w.set_focus();
    let _ = app.emit("usticky://quick-add", ());
}

/// toggle dismiss（快捷键 2nd press 调）：不隐藏窗口，仅还原 level + 切回原 app。
///
/// 顺序：restore level → activate prev app。
/// 注意：activate prev app 会让浮窗失焦 → 触发 Focused(false) 事件 →
/// 但此时 QUICK_ADD_ACTIVE 已经是 false，blur_dismiss 是 no-op，不会重复处理。
pub fn toggle_dismiss_floating_window(app: &tauri::AppHandle, store: &SharedStore) {
    clear_quick_add_state(app, store);
    platform::activate_previous_app_after_quick_add();
}

/// blur dismiss（窗口失焦事件调）：仅还原 level + 清状态。
/// **不** activate prev app —— 用户已经点了别处，不该抢焦点回去。
pub fn blur_dismiss_floating_window(app: &tauri::AppHandle, store: &SharedStore) {
    clear_quick_add_state(app, store);
}

/// hide dismiss（hide_floating_window 命令 / tray toggle hide / Esc 调）：
/// hide + 还原 level + 切回原 app。
///
/// 顺序：hide → restore level（必须在 hide 之后，否则 PinBottom 模式下浮窗
/// 先从 FLOATING 降到 -1 还显示一帧才隐藏，视觉上会闪一下被其他 app 盖住的画面）
/// → activate prev app。
pub fn hide_dismiss_floating_window(app: &tauri::AppHandle, store: &SharedStore) {
    let was_active = QUICK_ADD_ACTIVE.swap(false, Ordering::SeqCst);
    if let Some(w) = app.get_webview_window("floating") {
        let _ = w.hide();
    }
    if was_active {
        let mode = store.blocking_read().pin_mode();
        platform::restore_level_after_quick_add(app, mode);
        platform::activate_previous_app_after_quick_add();
    }
}

// **P3-7 fix**：clear_quick_add_active helper 在 P1-5 之后没人调用。
// show_floating_window_normal 直接 QUICK_ADD_ACTIVE.store(false, ...)
// 反而更明确（helper 多一层间接），删除 helper。

/// **P1-5 fix**："普通 show"浮窗——只 raise + show + focus，**不**激活
/// QUICK_ADD_ACTIVE，不 save prev app。
///
/// 跟 `quick_show_floating_window` 的区别：后者走全局快捷键路径（用户期待
/// "我按了快捷键所以窗口从我身后出现 → 切回时再回原 app"），前者是用户从
/// 设置面板 / 托盘主动"打开浮窗"按钮——用户期望"就显示在当前位置，不
/// 切走原 app focus"。
///
/// **不**激活 QUICK_ADD_ACTIVE 的关键意义：QUICK_ADD_ACTIVE=true 会让
/// `WindowEvent::Focused(false)` 触发 `blur_dismiss_floating_window` → 还原
/// level 到 PinBottom 的 -1 → 浮窗被任何前台 app 盖住。这是"用户点完打开
/// 浮窗 → 切到别的 app → 浮窗被盖住"的根因（违反用户"显示浮窗"的意图）。
///
/// 适用入口：
///   - `show_floating_window` IPC 命令（设置面板"打开浮窗"按钮）
///   - tray 左键单击的 show 分支
pub fn show_floating_window_normal(app: &tauri::AppHandle) {
    let Some(w) = app.get_webview_window("floating") else { return };
    // 保留 pin mode 原生 level（不 raise 到 FLOATING）—— PinBottom 用户
    // 主动打开浮窗时也希望它默认贴在桌面底部（hover 才临时置顶），不抢
    // 前台 app 的位置感。只 show + focus 已经够。
    QUICK_ADD_ACTIVE.store(false, Ordering::SeqCst);
    let _ = w.show();
    let _ = w.set_focus();
    // **不** emit usticky://quick-add —— 那是"快捷键唤起"专用的视觉激活
    // 信号，普通 show 不该触发（避免用户从设置面板打开浮窗时意外触发
    // active 90s timeout 状态机）。
}

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
        // **P3-5 fix**：autostart + notification 插件 v0.1 未使用，移除依赖
        // 减少二进制体积 + 启动时间。v0.2 真要做再添加。
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
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

            // 3. 注册全局快捷键（quick-add）：从 store 读 accelerator 字符串，
            //    走 [`parse_shortcut`] 解析（macOS 上 `Cmd` → SUPER / ⌘）。
            //    旧代码硬编码 `Modifiers::CONTROL | SHIFT`，在 macOS 上注册的
            //    是 ⌃⇧Space 而不是 ⌘⇧Space —— 这是 AGENTS.md 写的快捷键
            //    "没生效"的根因。改成字符串解析后用户可自行改键。
            register_quick_add_shortcut(app.handle(), &store);

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
                            // **P3-4 fix**：update_window_pos 用短暂的 write guard，
                            // 之后立刻 drop；persist 走 path clone + 短暂 read guard，
                            // 调完立刻 drop —— 不跨 I/O 持锁。
                            {
                                let mut s = store.write().await;
                                s.update_window_pos(Some(x), Some(y));
                            } // drop write guard 在 await 之间
                            let path = store.read().await.data_path_clone();
                            if let Some(p) = path {
                                if let Err(e) = store.read().await.persist_to_path(&p) {
                                    tracing::error!("persist window pos failed: {}", e);
                                    let _ = app.emit("usticky://persist-failed", e.to_string());
                                }
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
                            let path = store.read().await.data_path_clone();
                            if let Some(p) = path {
                                if let Err(e) = store.read().await.persist_to_path(&p) {
                                    tracing::error!("persist window size failed: {}", e);
                                    let _ = app.emit("usticky://persist-failed", e.to_string());
                                }
                            }
                        });
                    }
                    tauri::WindowEvent::CloseRequested { api, .. } => {
                        // 点 X 不退出 app，浮窗进 hide 状态（Musage 经验）
                        api.prevent_close();
                        let _ = window_for_close.hide();
                    }
                    tauri::WindowEvent::Focused(false) => {
                        // 浮窗失焦 —— 若处于 quick-add 临时置顶状态，还原 level
                        // （**不** activate prev app：用户已经点了别处，不该抢焦点回去）
                        let app = app_handle_geom.clone();
                        let store = store_for_geom.clone();
                        tauri::async_runtime::spawn(async move {
                            blur_dismiss_floating_window(&app, &store);
                        });
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

            // 8. pin mode 切换链路：tray 子菜单的 checkmark 要跟着刷新
            //    （浮窗 foot / 设置面板 / tray 子菜单任一处改 pin mode 都会 emit）
            let app_for_pin = app.handle().clone();
            app.listen("usticky://pin-mode-changed", move |_| {
                if let Err(e) = tray::rebuild_tray(&app_for_pin) {
                    tracing::warn!(error = %e, "rebuild_tray (pin mode) 失败");
                }
            });

            // 9. quick-add 快捷键切换链路：tray 子菜单显示当前快捷键的 label
            //    要跟着刷新。设置面板 + 浮窗 input hint 也通过这个事件同步。
            let app_for_sc = app.handle().clone();
            app.listen("usticky://shortcut-changed", move |_| {
                if let Err(e) = tray::rebuild_tray(&app_for_sc) {
                    tracing::warn!(error = %e, "rebuild_tray (shortcut) 失败");
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
            commands::get_quick_add_shortcut,
            commands::set_quick_add_shortcut,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Usticky");
}