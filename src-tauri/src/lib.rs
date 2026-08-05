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
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tauri::{Emitter, Listener, Manager};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};
use tokio::sync::Notify;

// rust_i18n crate 级初始化 —— 让 commands / tray 等模块都能直接 t!("xxx")。
// 文件放在 src-tauri/locales/{en,zh-CN}.json，跟前端 en.json / zh-CN.json
// 解耦（rust_i18n 不支持嵌套 dotted key，跟前端 dict 分开维护）。
rust_i18n::i18n!("locales");

mod clipboard;
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

/// **P2-8 fix**：浮窗 Moved/Resized 事件的 trailing-edge debounce 通道。
///
/// 旧实现每个事件 spawn 一个 task 调 persist，macOS 拖窗时 ~60Hz 派发
/// Moved/Resized → 每秒 ~120 次 persist(tmp write + sync + rename + parent
/// fsync) → SSD 写入放大 + 偶尔跟 add_todo 抢 store lock 触发 jank。
///
/// 新实现：
///   - Moved/Resized handler：拿 write guard 写 in-memory state → drop → notify_one()
///   - 单个 background task（app 启动时 spawn）监听 Notify，trailing-edge 200ms
///     debounce（每次新 notify 重置 timer），timeout 后用最新 store state 调
///     裸 [`persist_to_disk`]
///
/// 200ms 是经验值：拖窗结束（mouseup）后用户视觉上看到"窗口停住"到实际
/// 完成落盘的感知阈值约 100ms；200ms 留 buffer 又不至于让"拖完忘关 app"
/// 场景下窗口位置迟迟不存。
fn geom_notify() -> &'static Notify {
    static GEOM_NOTIFY: OnceLock<Notify> = OnceLock::new();
    GEOM_NOTIFY.get_or_init(Notify::new)
}

/// Moved/Resized handler 调用 —— 不阻塞 UI 线程，仅发信号。
fn notify_geom_changed() {
    geom_notify().notify_one();
}

/// **P2-8 fix**：浮窗几何 trailing-edge debounce 持久化后台循环。
///
/// 用 `Notify` + `tokio::time::sleep` 的 trailing-edge debounce：
///   - 等到第一个 notify → 起 200ms timer
///   - timer 期间再来 notify → 重置 timer
///   - timer 完成 → 拿 store 最新 snapshot → 调裸 persist_to_disk
///
/// 实现细节：内层 `loop + tokio::select!` 让 timer 在新 notify 到来时被
/// drop + 重建（select 的 sleep 分支走 break，外层 loop 重新 select 走 notify
/// 分支 + reset timer）。
async fn geom_persist_loop(store: SharedStore, app: tauri::AppHandle) {
    let notify = geom_notify();
    loop {
        // 等到第一个 Moved/Resized 信号
        notify.notified().await;
        // 内层 debounce loop：200ms 内持续有新信号就重置 timer
        loop {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(200)) => {
                    // 200ms 静默期 → trailing edge，break 去 persist
                    break;
                }
                _ = notify.notified() => {
                    // 期间又来信号 → 继续 loop 重置 timer（重新 select + 新 sleep）
                    continue;
                }
            }
        }
        // trailing edge → 落盘
        let (path, data) = {
            let s = store.read().await;
            (s.data_path_clone(), s.data_clone())
        };
        match path {
            Some(p) => {
                if let Err(e) = crate::todo::persist_to_disk(&p, &data) {
                    tracing::error!("debounced geom persist failed: {}", e);
                    let _ = app.emit("usticky://persist-failed", e.to_string());
                }
            }
            None => {
                tracing::error!("debounced geom persist: data_path 未初始化");
                let _ = app.emit("usticky://persist-failed", "data path not initialized");
            }
        }
    }
}

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

/// 注册当前 store 里的 quick-add 快捷键。`previous` 是回退用的——
/// 当 `on_shortcut` 失败时（最常见：快捷键被别的 app 占用），best-effort
/// 用同一 handler 把 `previous` 重新装回去，**不让用户失去快捷键能力**。
/// `previous = None` 用于启动时（OS 上没旧绑定可回退）。
///
/// **P1-3 fix**：原流程是"unregister_all → parse → on_shortcut"，导致
/// parse 失败时旧绑定已经被清掉。新流程"parse → unregister_all → on_shortcut"
/// 把 parse 提前，parse 失败时旧绑定保持不动；on_shortcut 失败时再
/// best-effort 用 `previous` 重新装回去。
///
/// 失败不致命（极端情况下用户存了个 parse 不出来的字符串 / 系统占用了快捷键）
/// —— log + emit `usticky://persist-failed` 让前端提示，但 app 继续跑
/// （快捷键只是不可用）。
fn register_quick_add_shortcut(
    app: &tauri::AppHandle,
    store: &SharedStore,
    previous: Option<&str>,
) {
    let accelerator = store.blocking_read().quick_add_shortcut();
    // 1. parse 先做 —— 失败时旧的 OS 绑定保持不动
    let parsed = match parse_shortcut(&accelerator) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("register_quick_add_shortcut parse failed: {}", e);
            let _ = app.emit("usticky://persist-failed", e);
            return;
        }
    };
    let gs = app.global_shortcut();
    // 2. parse OK → 清掉旧绑定（plugin 范围内）
    let _ = gs.unregister_all();
    // 3. 注册新绑定
    let app_handle = app.clone();
    let store_ref = store.clone();
    let register_res = gs.on_shortcut(parsed, move |_app, _shortcut, event| {
        if event.state() != ShortcutState::Pressed {
            return;
        }
        // **P1-9 fix**：原子声明活跃位。compare_exchange(false, true) 成功 = 唯一
        // 调用方，失败 = 已有别人持有活跃位（OS 抖动 / 重复触发 / 自己刚
        // toggle dismiss 完还没彻底清状态都会触发）。失败时走 toggle dismiss
        // 路径 —— 跟用户的"再次按 = 收起"心智模型一致。
        match QUICK_ADD_ACTIVE.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => {
                // ── show 分支：原子声明成功，独占 show_dismiss 状态机 ──
                quick_show_floating_window(&app_handle, &store_ref);
            }
            Err(_) => {
                // ── toggle dismiss 分支：别人持有活跃位 → 不隐藏窗口，
                //    只还原 level + 切回原 app + 清状态。
                toggle_dismiss_floating_window(&app_handle, &store_ref);
            }
        }
    });
    if let Err(e) = register_res {
        tracing::error!("on_shortcut failed: {}", e);
        let _ = app.emit(
            "usticky://persist-failed",
            format!("register shortcut: {e}"),
        );
        // 4. best-effort 装回 previous（用同一份 handler 闭包，inline 二次
        //    注册）。常见失败原因：快捷键被其他 app 占用 —— 此时装回
        //    previous 也很可能失败（previous 通常也冲突），但至少给用户
        //    一个可点开的 quick-add 入口（设置面板里有手动唤起路径）。
        if let Some(prev) = previous {
            if let Ok(parsed_prev) = parse_shortcut(prev) {
                let app2 = app.clone();
                let store2 = store.clone();
                match gs.on_shortcut(parsed_prev, move |_app, _shortcut, event| {
                    if event.state() != ShortcutState::Pressed { return; }
                    if QUICK_ADD_ACTIVE.load(Ordering::SeqCst) {
                        toggle_dismiss_floating_window(&app2, &store2);
                        return;
                    }
                    quick_show_floating_window(&app2, &store2);
                }) {
                    Ok(_) => tracing::warn!("restored previous shortcut {}", prev),
                    Err(e2) => tracing::error!(
                        "restore previous shortcut {} failed: {} (likely conflicting with another app)",
                        prev, e2
                    ),
                }
            } else {
                tracing::warn!("previous shortcut {} not parseable, cannot restore", prev);
            }
        }
    }
}

// **P1-9 fix**：全局快捷键 handler 改用 `compare_exchange` 原子声明活跃位。
// 旧实现是 `QUICK_ADD_ACTIVE.load()` → 分支判断 → `store(true)`，两次操作
// 不是原子的：同一快捷键在 macOS 上 Press+Release 序列触发多次回调、或
// 系统快速重复触发时，**两个**回调都看到 `false` → 两次都走 show 分支，
// 同时 `store(true)` 两次 —— show_dismiss 状态机被打乱、Focused(false) 触发
// 后无法回到正确状态。
//
// 改为 `compare_exchange(false, true, SeqCst, SeqCst)`：
//   - Ok 时才走 show 路径 —— 唯一调用方成功声明活跃位
//   - Err 时走 toggle dismiss 路径 —— 已有别人持有活跃位
//
// 这跟系统级 toggling 的语义对齐（再次按 = dismiss），同时让 OS 抖动不再
// 让状态机卡死。
// 注：compare_exchange 是 [`AtomicBool`] 上的方法，handler 闭包调用方只
// 需要 import 该 trait（通过 std::sync::atomic::AtomicBool 已 re-export）。

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
    let Some(w) = app.get_webview_window("floating") else {
        return;
    };
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
    // v0.2：浮窗 hide 时预览窗口一并收掉 —— always-on-top 的预览留在
    // 屏幕上而宿主浮窗消失，是无依孤儿窗。
    if let Some(p) = app.get_webview_window("preview") {
        let _ = p.close();
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
    let Some(w) = app.get_webview_window("floating") else {
        return;
    };
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
        // v0.2 剪贴板粘贴：Rust 端 read_text / read_image（RGBA 兜底路径，
        // macOS GIF 原字节走 clipboard.rs 的 NSPasteboard 路径）。
        .plugin(tauri_plugin_clipboard_manager::init())
        // **P2-6 fix**：single-instance 插件。第二次启动（用户双击图标 / 命令行
        // 唤起）时，**不**新开进程 —— 而是回调第一次的 app.handle()，让我们
        // 找到浮窗 + show + focus，让用户看到已有窗口而不是被新进程抢焦点。
        // 不做这个会触发两个并发进程同时持有 store / 同时写 todos.json 的
        // 灾难（macOS 上还会重复 dock 图标）。
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(w) = app.get_webview_window("floating") {
                let _ = w.show();
                let _ = w.set_focus();
            } else {
                // 浮窗被 close-hide 后可能 webview handle 还在但 hide 状态 ——
                // unminimize + set_focus 双保险。
                tracing::warn!("single-instance callback: floating window not found");
            }
        }))
        .setup(|app| {
            // 1. 加载或初始化 todo store
            let store = Store::load_or_init(app.handle()).expect("failed to init todo store");
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

            // **P2-8 fix**：启动 trailing-edge debounce geom persist 后台循环。
            // 该 task 与 app 同生命周期 —— app.exit() 后由 tauri::async_runtime
            // drop，loop 在 await 处终止。spawn 用当前 store / app 引用，
            // 后续 store.write 不需要再传引用。
            {
                let store_for_geom_loop = store.clone();
                let app_for_geom_loop = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    geom_persist_loop(store_for_geom_loop, app_for_geom_loop).await;
                });
            }

            // 3. 注册全局快捷键（quick-add）：从 store 读 accelerator 字符串，
            //    走 [`parse_shortcut`] 解析（macOS 上 `Cmd` → SUPER / ⌘）。
            //    旧代码硬编码 `Modifiers::CONTROL | SHIFT`，在 macOS 上注册的
            //    是 ⌃⇧Space 而不是 ⌘⇧Space —— 这是 AGENTS.md 写的快捷键
            //    "没生效"的根因。改成字符串解析后用户可自行改键。
            //    启动时 previous=None —— OS 上还没绑过任何快捷键，回退无意义。
            register_quick_add_shortcut(app.handle(), &store, None);

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
                        // **P2-8 fix**：in-memory 写立即（write guard 短暂），
                        // persist 交给 [`geom_persist_loop`] 后台 task 做
                        // trailing-edge debounce —— 60Hz Moved 不会触发 60Hz 落盘。
                        let store = store_for_geom.clone();
                        let (x, y) = (pos.x, pos.y);
                        tauri::async_runtime::spawn(async move {
                            {
                                let mut s = store.write().await;
                                s.update_window_pos(Some(x), Some(y));
                            }
                            notify_geom_changed();
                        });
                    }
                    tauri::WindowEvent::Resized(size) => {
                        // 过滤掉 (0, 0) —— 启动前 fire 的占位 resize
                        if size.width <= 0 || size.height <= 0 {
                            return;
                        }
                        // **P2-8 fix**：同 Moved，in-memory 写立即 + 发 debounce 信号。
                        let store = store_for_geom.clone();
                        let (w, h) = (size.width, size.height);
                        tauri::async_runtime::spawn(async move {
                            {
                                let mut s = store.write().await;
                                s.update_window_size(Some(w), Some(h));
                            }
                            notify_geom_changed();
                        });
                    }
                    tauri::WindowEvent::CloseRequested { api, .. } => {
                        // **P3-7 fix（注释明确）**：点 X **不**退出 app —— 浮窗进 hide 状态
                        // （Musage 经验），避免用户点错 X 失去所有工作。
                        //
                        // 跟 app.exit(0) 的关系：tray 的 "Quit Usticky" 走的是
                        // `app.exit(0)`，tauri 框架会先发 WindowEvent::CloseRequested
                        // 给所有 webview —— 此时 prevent_close() 已经把"X 关闭"路径
                        // 拦下来 hide，但 app.exit(0) **会**绕过 prevent_close()
                        // 强制 terminate（exit 路径不走 webview event）。所以这里
                        // 看似矛盾的"prevent_close + hide"不会拦下 tray quit。
                        //
                        // 行为：
                        //   - 用户点 X → prevent_close + hide（保持后台运行）
                        //   - 用户从 tray Quit → app.exit(0) → terminate
                        //   - 用户从 Cmd+Q（macOS） → app.exit(0) → terminate
                        // 这是有意设计，不是 bug。
                        api.prevent_close();
                        let _ = window_for_close.hide();
                        // v0.2：同 hide_dismiss —— 浮窗 hide 时收掉预览窗（孤儿窗）。
                        if let Some(p) = app_handle_geom.get_webview_window("preview") {
                            let _ = p.close();
                        }
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
            commands::set_cursor_pointer,
            commands::open_settings_window,
            commands::get_quick_add_shortcut,
            commands::set_quick_add_shortcut,
            commands::get_attachments_dir,
            commands::paste_from_clipboard,
            commands::open_preview_window,
            commands::close_preview_window,
            commands::prewarm_preview_window,
            commands::take_pending_preview_todo,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Usticky");
}
