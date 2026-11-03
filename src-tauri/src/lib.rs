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

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tauri::{Emitter, Listener, Manager};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut};
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

/// **P-rapid-press fix（2026-08-24）**：quick_show 单调计数器。每次
/// `quick_show_floating_window` 成功把 `QUICK_ADD_ACTIVE` 翻 true 时增 1。
///
/// 解决的问题：`Focused(false)` handler spawn 的 blur_dismiss 任务**可能延后
/// 几十 ms 才跑**（toki 调度 + blocking_read 卡顿），期间用户可能又按了
/// 几次快捷键（quick_show → toggle_dismiss 交替），任务的 `clear_quick_add_state`
/// 拿到 `was_active=true`（来自最新一次 quick_show）→ 误派发 LOWER。
///
/// 主线程 dispatch 队列形如 `[RAISE(按N), LOWER(stale任务)]` → 最终落到
/// BELOW_NORMAL，浮窗被其它 app 完全盖住，用户视觉等同"闪退"。
///
/// `clear_quick_add_state` 在 swap 之后再看一眼 epoch：若 epoch 变了说明
/// 中间至少夹了一次新 show，整个 restore 跳过（不派 LOWER 也不调
/// `set_window_pin_bottom` 翻 `LEVEL_SWITCHING_ACTIVE`，避免把 hover emitter
/// 的开关再打乱）。
///
/// 注：epoch 检查和后续 `restore_level_after_quick_add` 调用之间还有极小竞态
/// 窗口（其他线程可能再触发一次 quick_show）。要让 main thread 真正原子地
/// "check-then-set level"，得把检查挪进 `set_window_level` 的 dispatch 闭包里
/// —— 那需要给 set_window_level 加新参数，影响面更大。当前简单修复覆盖 99%
/// 场景（用户连续按 N 次只看到 N-1 次 stale restore），剩余小窗口实际很难撞上。
static SHOW_EPOCH: AtomicU64 = AtomicU64::new(0);

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

/// 显示器几何快照（物理 px）：全屏 bounds + work_area（扣菜单栏/Dock）。
/// 从 `tauri::Monitor` 抽成纯数据，让 [`clamp_restore_pos`] 可脱离
/// AppHandle 单测。
#[derive(Clone, Copy, Debug)]
struct MonitorRect {
    /// 全屏矩形（x, y, w, h，含菜单栏/Dock 区域）—— 点命中 / 最近屏判定用
    bounds: (i32, i32, i32, i32),
    /// 可视区矩形（同序）—— 最终 clamp 用（P0-5 同款语义，防菜单栏/Dock 遮）
    work: (i32, i32, i32, i32),
}

/// **v0.2.9 fix（双屏重启后浮窗骑在两屏交界处）**：多屏感知的启动位置恢复。
///
/// 旧实现把保存的 (x, y) clamp 到**主显示器**边界 —— 本意是"副屏拔了别把
/// 窗口扔屏外"，但它不认识副屏：窗口上次合法地在副屏（坐标超出主屏范围），
/// 重启后 `x.clamp(主屏区间)` 强行把它拉到主屏边缘，恰好落在双屏并排的
/// **交界处**。正确语义：
///   1. 保存坐标仍落在某块显示器**全屏矩形**内 → 恢复到那块屏
///   2. 所有屏都不包含该点（上次用的屏被拔 / 排列变了）→ 选**最近**的屏
///      （点 clamp 进各屏矩形算距离平方取最小）
///   3. monitors 为空（极端：API 失败）→ 返 None，调用方放弃 clamp 用原坐标
///   4. 无论哪条路径命中，最终都按**整窗收纳** clamp：窗口 (win_w × win_h)
///      放得下时保证**整窗**在该屏 work_area 内（左上角右界 = work 右界 -
///      win_w）；放不下（窗口比屏宽/高）才退化为"至少
///      [`RESTORE_MIN_VISIBLE_PX`] 可见"。
///      —— 第 4 条是首版修复的回归补丁：首版只保证左上角 60px 可见，
///      窗口实际 240-300px 宽，被钉在"屏右缘-60"后其余 240px 悬在屏外
///      没有显示器的空间里，用户看到的就是一条 ~35px 的"竖条"贴屏幕边。
///      整窗收纳同时治好"重启后骑在双屏缝上"的原始场景（左上角在 A 屏
///      但窗口伸进 B 屏 → 恢复时整窗拉回 A 屏）。
const RESTORE_MIN_VISIBLE_PX: i32 = 60;
fn clamp_restore_pos(
    monitors: &[MonitorRect],
    x: i32,
    y: i32,
    win_w: i32,
    win_h: i32,
) -> Option<(i32, i32)> {
    if monitors.is_empty() {
        return None;
    }
    // 1. 点命中判定（半开区间，右/下缘不属本屏 —— 并排屏共享边缘坐标）
    let hit = monitors.iter().position(|m| {
        let (rx, ry, rw, rh) = m.bounds;
        x >= rx && x < rx.saturating_add(rw) && y >= ry && y < ry.saturating_add(rh)
    });
    // 2. 都不命中 → 最近屏
    let idx = hit.or_else(|| {
        monitors
            .iter()
            .enumerate()
            .min_by_key(|(_, m)| {
                let (rx, ry, rw, rh) = m.bounds;
                let dx = x.clamp(rx, rx.saturating_add(rw).saturating_sub(1)) - x;
                let dy = y.clamp(ry, ry.saturating_add(rh).saturating_sub(1)) - y;
                dx * dx + dy * dy
            })
            .map(|(i, _)| i)
    })?;
    // 3. 整窗收纳 clamp（见 doc comment 第 4 条）。win_w/h 非正（调用方
    //    拿不到尺寸）时按 0 处理 —— 区间退化为 [wx, wx+ww]，效果等于
    //    只约束左上角在屏内，不会 panic。
    let (wx, wy, ww, wh) = monitors[idx].work;
    let max_dx = if win_w > 0 && win_w <= ww {
        ww - win_w
    } else {
        (ww - RESTORE_MIN_VISIBLE_PX).max(0)
    };
    let max_dy = if win_h > 0 && win_h <= wh {
        wh - win_h
    } else {
        (wh - RESTORE_MIN_VISIBLE_PX).max(0)
    };
    Some((
        x.clamp(wx, wx.saturating_add(max_dx)),
        y.clamp(wy, wy.saturating_add(max_dy)),
    ))
}

/// 从 AppHandle 抽全部显示器的几何快照（setup 恢复用）。
fn collect_monitor_rects(app: &tauri::AppHandle) -> Vec<MonitorRect> {
    app.available_monitors()
        .map(|ms| {
            ms.iter()
                .map(|m| {
                    let p = m.position();
                    let s = m.size();
                    let wa = m.work_area();
                    MonitorRect {
                        bounds: (p.x, p.y, s.width as i32, s.height as i32),
                        work: (
                            wa.position.x,
                            wa.position.y,
                            wa.size.width as i32,
                            wa.size.height as i32,
                        ),
                    }
                })
                .collect()
        })
        .unwrap_or_default()
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
        // **P1-3 fix（2026-08-13 全量审查）**：写锁内取快照 + epoch。旧实现
        // read guard 取快照 + 锁外写盘，与 add_todo 等 CRUD persist 交错时，
        // 旧快照可能后写盘把刚添加的 todo 覆盖掉（内存有、磁盘没有，退出
        // 后静默丢失）。bump_and_snapshot + persist_to_disk 的 epoch 比对
        // 保证"最后一次写入必然携带最新状态"。
        let (path, data, epoch) = {
            let mut s = store.write().await;
            s.bump_and_snapshot()
        };
        match path {
            Some(p) => {
                if let Err(e) = crate::todo::persist_to_disk(&p, &data, epoch) {
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

/// 构造全局快捷键回调闭包。主注册 + fallback（previous）注册共用同一份
/// **逻辑**，避免 inline 重复引入 check-then-act 非原子模式（v0.1.5 P1-9
/// 已经在主路径用 compare_exchange 修了，回退路径 inline 时容易漏掉）。
///
/// **P3-7 fix（2026-08-13 全量审查）**：旧 fallback 路径用 `if QUICK_ADD_ACTIVE.load() {...} else {...}`
/// 两步检查，正是 lib.rs:230-244 注释里修掉的"两个回调都看到 false → 双走
/// show 分支"模式。现在两个分支都走 compare_exchange。
fn make_quick_add_handler(
    app: &tauri::AppHandle,
    store: &SharedStore,
) -> impl Fn(
    &tauri::AppHandle,
    &tauri_plugin_global_shortcut::Shortcut,
    tauri_plugin_global_shortcut::ShortcutEvent,
) + Send
       + Sync
       + 'static {
    let app_handle = app.clone();
    let store_ref = store.clone();
    move |_app, _shortcut, event| {
        if event.state() != tauri_plugin_global_shortcut::ShortcutState::Pressed {
            return;
        }
        // **P1-9 fix**：原子声明活跃位，与主路径一致。
        match QUICK_ADD_ACTIVE.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => quick_show_floating_window(&app_handle, &store_ref),
            Err(_) => toggle_dismiss_floating_window(&app_handle, &store_ref),
        }
    }
}
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
    // 3. 注册新绑定（共用工厂函数，逻辑与 fallback 一致）
    let register_res = gs.on_shortcut(parsed, make_quick_add_handler(app, store));
    if let Err(e) = register_res {
        tracing::error!("on_shortcut failed: {}", e);
        let _ = app.emit(
            "usticky://persist-failed",
            format!("register shortcut: {e}"),
        );
        // 4. best-effort 装回 previous（用同一 handler 闭包，inline 二次
        //    注册）。常见失败原因：快捷键被其他 app 占用 —— 此时装回
        //    previous 也很可能失败（previous 通常也冲突），但至少给用户
        //    一个可点开的 quick-add 入口（设置面板里有手动唤起路径）。
        if let Some(prev) = previous {
            if let Ok(parsed_prev) = parse_shortcut(prev) {
                if let Err(e2) = gs.on_shortcut(parsed_prev, make_quick_add_handler(app, store)) {
                    tracing::error!(
                        "restore previous shortcut {} failed: {} (likely conflicting with another app)",
                        prev, e2
                    );
                } else {
                    tracing::warn!("restored previous shortcut {}", prev);
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
///
/// **P-rapid-press fix**：在 swap 后 capture `SHOW_EPOCH`，blocking_read 后
/// 再 check 一次。若 epoch 变了 → 中间夹了新的 quick_show → 我们的 dismiss
/// 是 stale 的 → 整个 restore 放弃（详见 `SHOW_EPOCH` 注释）。
fn clear_quick_add_state(app: &tauri::AppHandle, store: &SharedStore) {
    let was_active = QUICK_ADD_ACTIVE.swap(false, Ordering::SeqCst);
    if was_active {
        let epoch = SHOW_EPOCH.load(Ordering::SeqCst);
        let mode = store.blocking_read().pin_mode();
        if SHOW_EPOCH.load(Ordering::SeqCst) != epoch {
            // 中间夹了一次新的 quick_show，我们的 dismiss 已经被它覆盖。
            // 新 show 已经把 level 升到 FLOATING 并派了 RAISE，我们不能再派
            // LOWER 把它的 RAISE 顶掉（main thread 是 FIFO dispatch）。
            return;
        }
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
    // **P-rapid-press fix**：成功拿回浮窗控制权后才 bump epoch。
    // 放在 swap 之后是为了让 stale 的 blur_dismiss 任务能看到 epoch 变化；
    // 放在 show/focus 之前是为了让任何派发的 RAISE 都在 epoch 增 1 之后。
    QUICK_ADD_ACTIVE.store(true, Ordering::SeqCst);
    SHOW_EPOCH.fetch_add(1, Ordering::SeqCst);
    // save prev app 只在快捷键路径有意图（tray 主动唤起不需要切回原 app，
    // activate_previous_app_after_quick_add 是 no-op 也不会报错 —— 但为了语义清晰，
    // 这里仍然 save：tray hide 走 hide_dismiss 会 activate prev app，跟快捷键一致）
    platform::save_previous_app_for_quick_add();
    platform::raise_for_quick_add(app);
    let _ = w.show();
    // **P-WKWebView-init-race fix（2026-08-25）**：浮窗启动后第一次按快捷键时
    // WKWebView 还在 init / 第一次 layout pass，连按 show + focus 可能让 NSWindow
    // 进入 invalid visibility state，WKWebView 已知会在这个时机 NSException → 进程 abort
    // （macOS 上 panic="abort" 直接终止）。修：第一次 focus 派发到下一个 main-thread
    // cycle，让 WKWebView 有机会把 show 处理完再 focus。后续 focus 走 sync，
    // UX 上用户无感（10~20ms 差异肉眼不可见）。
    //
    // 跨进程第一次 focus 后 `FIRST_FOCUS_DONE` 翻 true，本会话内不再 defer。
    // 如果将来想做更精细的"等 on_page_load 完再 focus"，参考 [lib.rs setup 段]
    // 的 `on_page_load` hook 注释。
    //
    // **P3-18 fix（2026-09-04 全量审查）**：`swap` 返回**旧值**（false=从未
    // focus 过 → defer；true=已 focus 过 → sync）。这是 AtomicBool 经典踩坑点，
    // 注释显式写明防后人改飞。
    if FIRST_FOCUS_DONE.swap(true, Ordering::SeqCst) {
        let _ = w.set_focus();
    } else {
        let app_for_focus = app.clone();
        let label_for_focus = w.label().to_string();
        let _ = app.run_on_main_thread(move || {
            if let Some(w) = app_for_focus.get_webview_window(&label_for_focus) {
                let _ = w.set_focus();
            }
        });
    }
    // **P4 perf fix**：通知 hover emitter 窗口已显示 → 恢复 tick。必须在 show()
    // 之后调用，因为 emitter 下次 tick 会读 WINDOW_VISIBLE。
    #[cfg(target_os = "macos")]
    {
        use std::sync::atomic::Ordering;
        crate::platform::macos::WINDOW_VISIBLE.store(true, Ordering::SeqCst);
    }
    let _ = app.emit("usticky://quick-add", ());
}

/// 浮窗启动后是否已完成过第一次 focus。用于 [quick_show_floating_window] 在第一次
/// 按快捷键时 defer focus 避免 WKWebView init race（详见该函数注释）。
static FIRST_FOCUS_DONE: AtomicBool = AtomicBool::new(false);

/// toggle dismiss（快捷键 2nd press 调）：不隐藏窗口，仅还原 level + 切回原 app。
///
/// 顺序：restore level → activate prev app。
/// 注意：activate prev app 会让浮窗失焦 → 触发 Focused(false) 事件 →
/// 但此时 QUICK_ADD_ACTIVE 已经是 false，blur_dismiss 是 no-op，不会重复处理。
pub fn toggle_dismiss_floating_window(app: &tauri::AppHandle, store: &SharedStore) {
    clear_quick_add_state(app, store);
    platform::activate_previous_app_after_quick_add(app);
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
    // **P4 perf fix**：通知 hover emitter 窗口已隐藏 → 跳过整个 tick（详见
    // platform::macos::WINDOW_VISIBLE 注释）。
    #[cfg(target_os = "macos")]
    {
        use std::sync::atomic::Ordering;
        crate::platform::macos::WINDOW_VISIBLE.store(false, Ordering::SeqCst);
    }
    // v0.2：浮窗 hide 时预览窗口一并收掉 —— always-on-top 的预览留在
    // 屏幕上而宿主浮窗消失，是无依孤儿窗。
    if app.get_webview_window("preview").is_some() {
        // **P3-4 fix（2026-08-13 全量审查）**：close 前先 emit preview-flush
        // + 200ms 延迟，让 preview.ts 把防抖中未落盘的 update_todo invoke
        // 走完。Tauri 2 在 macOS WKWebView 下偶发不触发 webview beforeunload
        // （v0.2.6 实证）→ 不 flush 会丢 <700ms 编辑。emit 后 spawn 异步
        // 延迟关闭，主路径立即返回。
        let _ = app.emit("usticky://preview-flush", ());
        let app2 = app.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            if let Some(p) = app2.get_webview_window("preview") {
                let _ = p.close();
            }
            let _ = app2.emit("usticky://preview-closed", ());
            let _ = app2.emit("usticky://backdrop-refresh", ());
            // **P1-4 fix（2026-08-13 全量审查）**：hide 路径直接 `p.close()`
            //  预览，不走 close_preview_window 命令（IPC 多跳），必须主动
            //  清 PENDING 槽位，否则下次 fresh-create 时陈旧 slot 致显示/
            //  编辑错误 todo。
            crate::commands::clear_pending_preview_todo();
        });
    }
    if was_active {
        let mode = store.blocking_read().pin_mode();
        platform::restore_level_after_quick_add(app, mode);
        platform::activate_previous_app_after_quick_add(app);
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
    // **P4 perf fix**：同 quick_show_floating_window —— 通知 hover emitter 恢复 tick。
    #[cfg(target_os = "macos")]
    {
        use std::sync::atomic::Ordering;
        crate::platform::macos::WINDOW_VISIBLE.store(true, Ordering::SeqCst);
    }
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
            // **P2-4 fix（2026-08-13 全量审查）**：旧实现裸 `show + set_focus`，
            // 默认 PinBottom（level=-1）下窗口被任何前台 app 盖住 —— 用户
            // 双击 Dock 再启动时"什么都没发生"。走 quick_show_floating_window
            // 先 raise 到 FLOATING 再 show。try_state 容错：setup 未完成时
            // store 尚未 manage（理论上 single-instance 不会在 setup 之前
            // 触发，但兜底）走 fallback bare-show。
            if let Some(store) = app.try_state::<SharedStore>() {
                quick_show_floating_window(app, store.inner());
            } else if let Some(w) = app.get_webview_window("floating") {
                tracing::warn!("single-instance callback: store not ready, bare show");
                let _ = w.show();
                let _ = w.set_focus();
            } else {
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
                // **v0.2.9 fix（双屏重启后浮窗骑在两屏交界处）**：多屏感知恢复。
                // 旧实现 clamp 到主显示器边界 —— 窗口上次在副屏（坐标超出
                // 主屏范围）时，重启后被强行拉到主屏边缘 = 双屏交界处。
                // 现在按"命中屏恢复 / 屏没了选最近屏 + **整窗收纳**进该屏
                // work_area"的语义走 [`clamp_restore_pos`]（详见其 doc comment，
                // 含首版"只保左上角 60px 可见 → 240px 宽窗口悬屏外露 35px
                // 竖条"的回归补丁说明）。
                let monitors = collect_monitor_rects(app.handle());
                // 整窗收纳需要窗口尺寸：优先用持久化值（跟位置同一生命周期），
                // 缺省退到当前 outer_size（窗口已按 config 默认尺寸建好）。
                let (win_w, win_h) = match (geom.width, geom.height) {
                    (Some(w), Some(h)) if w > 0 && h > 0 => (w as i32, h as i32),
                    _ => window
                        .outer_size()
                        .map(|s| (s.width as i32, s.height as i32))
                        .unwrap_or((300, 200)),
                };
                if let (Some(x), Some(y)) = (geom.x, geom.y) {
                    let (cx, cy) =
                        clamp_restore_pos(&monitors, x, y, win_w, win_h).unwrap_or((x, y));
                    if (cx, cy) != (x, y) {
                        tracing::debug!(
                            "启动恢复浮窗位置：saved=({x},{y}) → clamp=({cx},{cy})（{} 块屏, 窗口 {win_w}×{win_h}）",
                            monitors.len()
                        );
                    }
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
            // **P2-8 fix**：启动 trailing-edge debounce geom persist 后台循环。
            if let Some(window) = app.get_webview_window("floating") {
                let store_for_geom = store.clone();
                let app_handle_geom = app.handle().clone();
                window.on_window_event(move |event| match event {
                    tauri::WindowEvent::Moved(pos) => {
                        // **P2-8 fix**：in-memory 写立即（write guard 短暂），
                        // persist 交给 [`geom_persist_loop`] 后台 task 做
                        // trailing-edge debounce —— 60Hz Moved 不会触发 60Hz 落盘。
                        // **P1-10 fix（2026-09-04 全量审查）**：旧实现 spawn 一个
                        // task 拿 store.write() → macOS 拖窗 60Hz 时 60 个 task
                        // 排队抢 write guard，期间所有 IPC 命令（add_todo 等）排队。
                        // 改 try_write 同步快速路径：拿不到锁直接 skip（geom_persist_loop
                        // 200ms trailing 会自然补上，丢一帧 16ms 用户无感）。
                        let store = store_for_geom.clone();
                        let (x, y) = (pos.x, pos.y);
                        match store.try_write() {
                            Ok(mut s) => s.update_window_pos(Some(x), Some(y)),
                            Err(_) => {
                                tracing::trace!("Moved: try_write busy, skip（geom_persist_loop 兜底）");
                                return;
                            }
                        }
                        notify_geom_changed();
                    }
                    tauri::WindowEvent::Resized(size) => {
                        // 过滤掉 (0, 0) —— 启动前 fire 的占位 resize
                        if size.width <= 0 || size.height <= 0 {
                            return;
                        }
                        // **P2-8 fix** + **P1-10 fix**：同 Moved，try_write 同步
                        // 快速路径，避免 60Hz 时60 task 排队抢锁。
                        let store = store_for_geom.clone();
                        let (w, h) = (size.width, size.height);
                        match store.try_write() {
                            Ok(mut s) => s.update_window_size(Some(w), Some(h)),
                            Err(_) => {
                                tracing::trace!("Resized: try_write busy, skip");
                                return;
                            }
                        }
                        notify_geom_changed();
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
                        //
                        // **P2-1 + P1-5 fix（2026-08-13 全量审查）**：旧实现只
                        // `prevent_close + hide` + `p.close()`，但**不**清
                        // QUICK_ADD_ACTIVE / WINDOW_VISIBLE / 不还原 level，
                        // 导致：① 下次快捷键按一次无反应（flag 卡 true）；②
                        // 隐藏期间 hover emitter 在 macOS 上满速空转；③ 若
                        // 隐藏时是 hover-raised 状态（PinBottom），show 后浮窗
                        // 永久卡在 FLOATING 恒置顶直到鼠标划过。统一调
                        // `hide_dismiss_floating_window` 把 hide + 状态机复位
                        // + 预览窗清理 + level 还原放到一条路径。
                        //
                        // **P1-11 fix（2026-09-04 全量审查）**：hide_dismiss 内部
                        // blocking_read + emit 在 main thread 上同步 → fsync 卡顿
                        // 可被用户感知（关闭按钮点一次卡一下）。修：spawn 出去。
                        api.prevent_close();
                        let app = app_handle_geom.clone();
                        let store = store_for_geom.clone();
                        tauri::async_runtime::spawn(async move {
                            crate::hide_dismiss_floating_window(&app, &store);
                        });
                        // 注：async_runtime::spawn 不 block main thread，
                        // 但 ui thread 仍需 hide() 立即执行（用户期望"点 X 立刻
                        // 看不到浮窗"）。补一次同步 hide：
                        if let Some(floating) = app_handle_geom.get_webview_window("floating") {
                            let _ = floating.hide();
                        }
                        // 预览窗连锁关（hide_dismiss 内部也有，但 spawn 出去可能晚到）
                        if let Some(preview) = app_handle_geom.get_webview_window("preview") {
                            let _ = preview.close();
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
            commands::restore_todo,
            commands::purge_attachment,
            commands::reorder_todos,
            commands::resize_floating_window,
            commands::resize_preview_window,
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
            commands::paste_image_to_todo,
            commands::open_preview_window,
            commands::close_preview_window,
            commands::prewarm_preview_window,
            commands::take_pending_preview_todo,
            commands::pin_preview,
            commands::close_pinned_preview,
            commands::dump_perf,
            commands::get_perf_path,
        ])
        .build(tauri::generate_context!())
        .inspect_err(|e| tracing::error!("tauri build failed: {e}"))
        .expect("error while building Usticky")
        .run(|app_handle, event| {
            // **P3-8 fix（2026-08-13 全量审查）**：geom persist 是 trailing-edge
            // 200ms 后台循环，app.exit(0) 后 tokio runtime drop、task 在 await
            // 取消 —— 200ms 窗口内未落盘的几何位置静默丢弃。ExitRequested 阶段
            // （用户从 tray Quit / Cmd+Q）同步 bump + 写盘最后一批状态，关闭前
            // 完成持久化。
            if let tauri::RunEvent::ExitRequested { .. } = event {
                // **P1-12 fix（2026-09-04 全量审查）**：spawn 出 main thread，
                // 不让 fsync 吃掉 OS quit 宽限期。旧实现同步 blocking_write +
                // persist_to_disk（含 rename + parent fsync）在 main thread 上
                // —— SSD 1-50ms / HDD 50-500ms 卡顿可能让用户拖完窗后 quit
                // 最后位置丢失（OS 等不到宽限期内完成就 terminate）。修：spawn
                // 到 blocking task pool，但这里 RunEvent 没 app handle 持续性
                // —— 改 try_write 快速路径同步落盘，但**跳过 fsync**（让 OS
                // 自己 buffer flush）；用户退出后数据完整性由 next-boot 兜底
                // （todos.json 写完整但 parent fsync 缺 → 极端掉电可能丢；
                // 实际上 OS 退出前会刷一遍 page cache）。
                //
                // 注意：更激进的方案是 tokio::task::spawn_blocking，但 RunEvent
                // 闭包返回值是同步的（必须返 ()），spawn 不等结果就退出闭包
                // —— 此处先用 try_write 同步快速路径（拿不到锁直接 skip——
                // geom_persist_loop 200ms 内已落盘）。
                if let Some(store) = app_handle.try_state::<SharedStore>() {
                    let (path, data, epoch) = match store.try_write() {
                        Ok(mut s) => s.bump_and_snapshot(),
                        Err(_) => {
                            tracing::warn!("exit flush: try_write busy, skip（store 已被其他路径持有）");
                            return;
                        }
                    };
                    if let Some(p) = path {
                        if let Err(e) = crate::todo::persist_to_disk(&p, &data, epoch) {
                            tracing::warn!("exit flush persist failed: {e}");
                        }
                    }
                }
            }
        });
}

#[cfg(test)]
mod geom_restore_tests {
    use super::*;

    /// 典型双屏布局：主屏（含菜单栏 work_area）+ 右侧副屏（Retina 物理坐标）。
    fn dual_monitors() -> Vec<MonitorRect> {
        vec![
            MonitorRect {
                bounds: (0, 0, 2560, 1600),
                work: (0, 50, 2560, 1490),
            },
            MonitorRect {
                bounds: (2560, 0, 1440, 2560),
                work: (2560, 50, 1440, 2460),
            },
        ]
    }

    /// 典型浮窗尺寸（物理 px）：窄卡片 300 宽、内容高 ~800（31 条卡全屏高）。
    const WIN: (i32, i32) = (300, 800);

    /// **v0.2.9 核心回归**：窗口上次在**副屏**，重启后必须原样恢复 ——
    /// 旧实现 clamp 到主屏右缘（2560-50），恰好落在两屏交界处。
    #[test]
    fn restore_pos_on_secondary_monitor_kept() {
        let mons = dual_monitors();
        // 副屏内的合法坐标
        assert_eq!(
            clamp_restore_pos(&mons, 3000, 400, WIN.0, WIN.1),
            Some((3000, 400))
        );
        // 贴副屏左缘（就是交界线右侧）也原样保留
        assert_eq!(
            clamp_restore_pos(&mons, 2560, 400, WIN.0, WIN.1),
            Some((2560, 400))
        );
    }

    /// 主屏内坐标同样原样恢复（不能被新逻辑误伤）。
    #[test]
    fn restore_pos_on_primary_monitor_kept() {
        let mons = dual_monitors();
        assert_eq!(
            clamp_restore_pos(&mons, 200, 300, WIN.0, WIN.1),
            Some((200, 300))
        );
        assert_eq!(
            clamp_restore_pos(&mons, 0, 100, WIN.0, WIN.1),
            Some((0, 100))
        );
    }

    /// 保存坐标在菜单栏区域内（y < work_area 顶）→ clamp 到可视区顶。
    #[test]
    fn restore_pos_clamps_into_work_area() {
        let mons = dual_monitors();
        // y=10 在菜单栏里 → 顶到 work_area 顶 50
        assert_eq!(
            clamp_restore_pos(&mons, 200, 10, WIN.0, WIN.1),
            Some((200, 50))
        );
    }

    /// **竖条回归（v0.2.9 二修）**：副屏被拔后，保存坐标经"最近屏"分支
    /// 恢复 —— 必须按**整窗宽度**收纳（x = 主屏右界 - 300），不能只保证
    /// 左上角 60px 可见（首版 clamp 到 2560-60 → 300px 宽窗口有 240px
    /// 悬在屏外，用户只看到 35px 竖条）。
    #[test]
    fn restore_pos_from_unplugged_monitor_contains_whole_window() {
        let mons = vec![MonitorRect {
            bounds: (0, 0, 2560, 1600),
            work: (0, 50, 2560, 1490),
        }];
        // 上次在 (3000, 400)（副屏没了）→ 整窗收纳：x = 2560 - 300
        assert_eq!(
            clamp_restore_pos(&mons, 3000, 400, WIN.0, WIN.1),
            Some((2560 - 300, 400))
        );
        // 比 work_area 还靠下也一样被收纳（y 上界 = 50 + (1490-800) = 740）
        assert_eq!(
            clamp_restore_pos(&mons, 3000, 1400, WIN.0, WIN.1),
            Some((2560 - 300, 740))
        );
    }

    /// **骑缝回归（v0.2.9 二修）**：保存坐标命中 A 屏但窗口伸进 B 屏
    /// （骑双屏缝）→ 恢复时整窗拉回 A 屏内。只看左上角命中的首版会
    /// 原样保留骑缝位置。
    #[test]
    fn restore_seam_straddle_pulls_window_fully_onscreen() {
        let mons = dual_monitors();
        // 左上角 2510 在主屏（< 2560）但 300px 宽窗口跨进副屏 → 拉回整窗
        assert_eq!(
            clamp_restore_pos(&mons, 2510, 300, WIN.0, WIN.1),
            Some((2560 - 300, 300))
        );
    }

    /// 窗口比屏**高**（长列表全屏高 2000 > work 1490）：y 退化为"至少
    /// 顶部可见一条"（clamp 上界 = work 底 - 60），不会反转/panic。
    #[test]
    fn restore_taller_than_monitor_keeps_top_visible() {
        let mons = dual_monitors();
        assert_eq!(
            clamp_restore_pos(&mons, 200, 100, 300, 2000),
            Some((200, 100))
        );
        // y=1400 ≤ 上界 50+(1490-60)=1480 → 原样保留（顶部可见）
        assert_eq!(
            clamp_restore_pos(&mons, 200, 1400, 300, 2000),
            Some((200, 1400))
        );
        // y 更靠下才被压回 1480
        assert_eq!(
            clamp_restore_pos(&mons, 200, 1600, 300, 2000),
            Some((200, 1480))
        );
    }

    /// 副屏在主屏**左侧**（负坐标）：命中副屏原样恢复；副屏没了 clamp
    /// 到主屏左缘 0（不是负坐标扔屏外）。
    #[test]
    fn restore_pos_with_left_secondary_monitor() {
        let mons = vec![
            MonitorRect {
                bounds: (-1440, 0, 1440, 2560),
                work: (-1440, 50, 1440, 2460),
            },
            MonitorRect {
                bounds: (0, 0, 2560, 1600),
                work: (0, 50, 2560, 1490),
            },
        ];
        assert_eq!(
            clamp_restore_pos(&mons, -1000, 300, WIN.0, WIN.1),
            Some((-1000, 300))
        );
        // 副屏拔掉后（只剩主屏），旧坐标 (-1000, 300) → clamp 到主屏内
        let only_primary = vec![mons[1]];
        assert_eq!(
            clamp_restore_pos(&only_primary, -1000, 300, WIN.0, WIN.1),
            Some((0, 300))
        );
    }

    /// 显示器列表拿不到（API 失败）→ None，调用方用原坐标（不瞎 clamp）。
    #[test]
    fn restore_pos_with_no_monitors_returns_none() {
        assert_eq!(clamp_restore_pos(&[], 3000, 400, WIN.0, WIN.1), None);
    }

    /// 极端小屏（work_area 比 RESTORE_MIN_VISIBLE_PX 还窄）：clamp 区间
    /// 退化而非反转（lo > hi 时 i32::clamp 会 panic）。退化语义 = 贴
    /// work_area 右/下界（x 界 100-60=40、y 界退化到顶 10）。
    #[test]
    fn restore_pos_tiny_monitor_does_not_panic() {
        let mons = vec![MonitorRect {
            bounds: (0, 0, 100, 80),
            work: (0, 10, 100, 60),
        }];
        // 窗口 300 宽 > 屏宽 100 → 退化到 min-visible 语义
        assert_eq!(
            clamp_restore_pos(&mons, 50, 40, WIN.0, WIN.1),
            Some((40, 10))
        );
    }
}
