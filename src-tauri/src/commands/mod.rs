// IPC commands —— 暴露给前端的 #[tauri::command]
//
// 设计：commands 都很瘦，只做 (1) 拿 store 引用 (2) 调 store 方法 (3) emit
// todos-changed 事件。所有业务逻辑在 Store 里。
//
// DTO 全部 #[serde(rename_all = "camelCase")] —— Tauri 2 对 struct 字段
// 也走 camelCase 转换（Musage PR 1b 实测坑）。
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder};

use crate::todo::{PinMode, Todo, TodoSnapshot, TodoStatus};
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
///
/// **P1-2 fix**：一次拿 TodoSnapshot + StoreData + path 三个 clone，**立刻**
/// drop read guard，然后调裸 free function [`crate::todo::persist_to_disk`]
/// 写盘。整段 I/O 不持任何 RwLock，旧实现里 RwLockReadGuard 跨 fs write +
/// sync_all + rename（几十 ms）会让同时到来的 IPC 写命令排队等锁 ——
/// 拖窗 ~60Hz spawn Moved/Resized 任务 + 用户同时 add_todo 时会感知卡顿。
async fn persist_and_emit(app: &AppHandle, store: &SharedStore) -> TodoSnapshot {
    let (snap, data, path) = {
        let s = store.read().await;
        (s.snapshot(), s.data_clone(), s.data_path_clone())
    };
    match path {
        Some(p) => {
            if let Err(e) = crate::todo::persist_to_disk(&p, &data) {
                tracing::error!("persist failed: {}", e);
                let _ = app.emit("usticky://persist-failed", e.to_string());
            }
        }
        None => {
            tracing::error!("persist failed: data_path 未初始化");
            let _ = app.emit("usticky://persist-failed", "data path not initialized");
        }
    }
    emit_todos_changed(app, &snap);
    snap
}

/// 仅落盘（**不** emit todos-changed）—— 给 pin mode / locale / shortcut /
/// 窗口几何等"改了跟 todo 列表无关"的路径用。
///
/// **P1-2 fix**：跟 [`persist_and_emit`] 共享同一份"clone snapshot → drop guard
/// → 调裸 [`persist_to_disk`]"模板，零 RwLock 跨 I/O。
async fn persist_only(app: &AppHandle, store: &SharedStore) {
    let (path, data) = {
        let s = store.read().await;
        (s.data_path_clone(), s.data_clone())
    };
    match path {
        Some(p) => {
            if let Err(e) = crate::todo::persist_to_disk(&p, &data) {
                tracing::error!("persist failed: {}", e);
                let _ = app.emit("usticky://persist-failed", e.to_string());
            }
        }
        None => {
            tracing::error!("persist failed: data_path 未初始化");
            let _ = app.emit("usticky://persist-failed", "data path not initialized");
        }
    }
}

/// 状态字符串 → enum。非法值直接报错，让前端知道走错了路径。
fn parse_status(s: &str) -> Result<TodoStatus, String> {
    match s {
        "pending" => Ok(TodoStatus::Pending),
        "done" => Ok(TodoStatus::Done),
        other => Err(format!("invalid status: {}", other)),
    }
}

/// todo title 校验：trim + 非空 + ≤ 280 字符。
///
/// **P2-7 fix**：add_todo 已有这套校验，update_todo 之前**完全跳过**，
/// 用户（或前端 bug）可以把 title 改成空串 / 纯空格 / 超长串。
/// 抽到独立函数让 add/update 共用，避免校验规则分叉。
fn validate_title(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim().to_string();
    if trimmed.is_empty() {
        return Err(rust_i18n::t!("commands.error.empty_title").into());
    }
    if trimmed.chars().count() > 280 {
        return Err(rust_i18n::t!("commands.error.too_long").into());
    }
    Ok(trimmed)
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
    let trimmed = validate_title(&title)?;
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
    // **P2-7 fix**：校验 title。前端可能传 None（只改 status）或 Some(string)。
    // None 透传不校验；Some 必须走 validate_title，否则透传空串/超长串到 store。
    let title = match title {
        Some(t) => Some(validate_title(&t)?),
        None => None,
    };
    let status_enum = match status {
        Some(s) => Some(parse_status(&s)?),
        None => None,
    };
    let maybe_updated = {
        let mut s = store.write().await;
        // **P2-4 fix**：update 现在返 Result<Option<Todo>>。None = no-op
        // （title/status 都是 None 的误用），跳过 persist + emit，省一次
        // 磁盘 I/O 和一次 todos-changed 渲染。Some = 实际改了。
        s.update(&id, title, status_enum)
            .map_err(|e| e.to_string())?
    };
    match maybe_updated {
        Some(updated) => {
            persist_and_emit(&app, &store).await;
            Ok(updated)
        }
        None => {
            // No-op：fetch current state 返给前端，**不**触发 persist / emit。
            let cur = store
                .read()
                .await
                .todos()
                .iter()
                .find(|t| t.id == id)
                .cloned()
                .ok_or_else(|| rust_i18n::t!("commands.error.not_found").to_string())?;
            Ok(cur)
        }
    }
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
        // **P1-1 fix**：reorder 现在返 Result，非完整 section 子集直接 Err 上抛，
        // 前端拿到非 Ok 后不要 retry / render 脏数据。
        s.reorder(&ids).map_err(|e| e.to_string())?;
    }
    persist_and_emit(&app, &store).await;
    Ok(())
}

// ── 浮窗控制 ──

#[tauri::command]
pub async fn show_floating_window(
    app: AppHandle,
    _store: State<'_, SharedStore>,
) -> Result<(), String> {
    // **P1-5 fix**：走"普通 show"路径——只 show + focus，**不**激活
    // QUICK_ADD_ACTIVE。否则用户从设置面板/托盘主动"打开浮窗"后，
    // 切到别的 app → WindowEvent::Focused(false) → blur_dismiss 把 level
    // 还原到 PinBottom 的 -1 → 浮窗被盖住，违反用户意图。
    crate::show_floating_window_normal(&app);
    Ok(())
}

#[tauri::command]
pub async fn hide_floating_window(
    app: AppHandle,
    store: State<'_, SharedStore>,
) -> Result<(), String> {
    // hide 路径：hide + restore level + activate prev app（仅当 QUICK_ADD_ACTIVE=true）
    crate::hide_dismiss_floating_window(&app, store.inner());
    Ok(())
}

#[tauri::command]
pub async fn reset_floating_window(
    app: AppHandle,
    store: State<'_, SharedStore>,
) -> Result<(), String> {
    // **fix 2026-07-17**：用 OS 主显示器（菜单栏所在 / Windows 标记为
    // "Main display" 的屏），**不**用 current_monitor。无论用户把浮窗
    // 拖到哪个副屏，点重置都回主屏中央——这是用户的产品预期（"重置"
    // = 回到默认位置 = 主屏正中央）。
    //
    // 选用 AppHandle::primary_monitor()（tao 在 macOS 上走
    // CGMainDisplayID()，Windows 上走 MONITORINFOF_PRIMARY 标志的屏，
    // 跟窗口当前位置无关）。
    //
    // Wayland 上 primary_monitor() 可能返 None（tao 历史 panic 改 None），
    // fallback 到 available_monitors().first()（= 当前拿到的第一个 monitor，
    // 不一定是 OS 主屏，但比崩溃好）。
    let monitor = app
        .primary_monitor()
        .map_err(|e| e.to_string())?
        .or_else(|| {
            app.available_monitors()
                .ok()
                .and_then(|m| m.into_iter().next())
        })
        .ok_or_else(|| rust_i18n::t!("commands.error.no_primary_monitor").to_string())?;
    let mon_size = monitor.size();
    let mon_pos = monitor.position();
    if let Some(w) = app.get_webview_window("floating") {
        // **P2-12 fix**：用持久化的 (w, h)（来自上次 Resized 事件 + 跨重启）
        // 而不是当前 outer_size。后者会拿"现在显示的尺寸"——若用户曾在浮窗
        // hide 时被 macOS 自动 resize 过、或副屏拔了被 fallback 到主屏缩放，
        // outer_size 跟用户实际想保存的不一致。持久化窗口尺寸跟位置是同一
        // 个生命周期（来自 WindowGeom），一起用更连贯。
        let (win_w, win_h) = {
            let geom = store.read().await.last_window_geom().clone();
            match (geom.width, geom.height) {
                (Some(pw), Some(ph)) if pw > 0 && ph > 0 => (pw, ph),
                _ => {
                    let cur = w.outer_size().map_err(|e| e.to_string())?;
                    (cur.width, cur.height)
                }
            }
        };
        let x = mon_pos.x + ((mon_size.width as i32 - win_w as i32) / 2);
        let y = mon_pos.y + ((mon_size.height as i32 - win_h as i32) / 2);
        tracing::debug!(
            "reset_floating_window: 目标显示器 pos=({}, {}) size={}x{} → 窗口新位置 ({}, {}) size={}x{}",
            mon_pos.x, mon_pos.y, mon_size.width, mon_size.height, x, y, win_w, win_h
        );

        // **P2-16 fix**：浮窗 hide 时**不**调 set_position。macOS 上
        // set_position 会把 hidden window 提到 front → 用户"明明没主动开
        // 却被浮窗挡住"。改成只持久化新 (x, y)，下次 show() 时由 setup 流程
        // 启动时或下次 Resized 时自然应用。
        let is_visible = w.is_visible().unwrap_or(false);
        if is_visible {
            w.set_position(tauri::PhysicalPosition::new(x, y))
                .map_err(|e| e.to_string())?;
        }

        {
            let mut s = store.write().await;
            s.update_window_pos(Some(x), Some(y));
            // **P2-12 fix**：同时把当前计算出的尺寸写回 store，让持久化
            // 跟"用户上次实际看到的尺寸"对齐——防止下次启动 restore 时
            // outer_size / persisted size 出现 drift。
            s.update_window_size(Some(win_w), Some(win_h));
        }

        // **P1-2 fix**：调裸 persist_only helper（无 RwLock 跨 I/O）。
        persist_only(&app, store.inner()).await;

        // **P2-19 fix**：emit `usticky://window-pos-changed` 给浮窗 webview
        // （reset 后浮窗的位置已变，前端 cached 的"outer_pos"和 CSS 渲染
        // 都需要刷新；不影响其他 webview）。set_position 成功 OR 持久化成功
        // 都 emit —— hidden 分支没调 set_position 但用户期望"按了重置按钮
        // 后下次 show 的位置就是这里算的"。
        if let Some(floating) = app.get_webview_window("floating") {
            let _ = floating.emit(
                "usticky://window-pos-changed",
                serde_json::json!({
                    "x": x, "y": y, "w": win_w, "h": win_h,
                }),
            );
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

        // 浮窗**顶部钉死 + 底部不超过屏幕底**：cur_y 是浮窗当前顶部 y
        // （用户拖到哪锁哪），max_h = 屏幕底 - 顶部位置 - 12px 喘息 → 顶部
        // 不动，底部能到哪到哪。超出部分由 #app 内部 overflow-y + wheel
        // 滚动兜底（macOS NSPanel wheel 派发问题见 main.ts wheel handler）。
        // min(mon_size.height) 防御 cur_y 拖到屏幕上沿时 max 越界；
        // max(160) 兜底不让窗口被压没（与 tauri.conf.json minHeight 对齐）。
        let mon = w
            .current_monitor()
            .map_err(|e| e.to_string())?
            .or_else(|| app.primary_monitor().ok().flatten())
            .ok_or_else(|| "no monitor for floating window".to_string())?;
        let mon_pos = mon.position();
        let mon_size = mon.size();
        const BOTTOM_MARGIN_PX: i32 = 12;
        let cur_y = w.outer_position().map_err(|e| e.to_string())?.y;
        let max_h_in_mon = ((mon_pos.y + mon_size.height as i32 - BOTTOM_MARGIN_PX - cur_y)
            .min(mon_size.height as i32)
            .max(160)) as u32;
        let final_h = new_h_physical.min(max_h_in_mon);
        // width 沿用 outer_size，**不**调 set_position —— 顶部不动是用户的
        // 预期，resize 不能改 y。
        w.set_size(tauri::PhysicalSize::new(cur.width, final_h))
            .map_err(|e| e.to_string())?;
        // 是否触底：前后高度都被 clamp 到 max（之前 height 也已贴到 max，
        // 这次想涨但被压回 max）。这告诉前端："最后一行可能被 macOS
        // dock 栏遮挡，加一段底部 padding 让 wheel 滚到底时还有喘息"。
        //
        // 不要 emit 之前未 bottomed 但本次仍然没 bottomed 的情况 —— 那
        // 是一次无效事件，前端 toggle 没意义还可能动画闪烁。所以只在
        // **进入 / 离开** bottomed 状态时 emit。
        let was_bounded = cur.height >= max_h_in_mon;
        let is_bounded = final_h >= max_h_in_mon;
        if was_bounded != is_bounded {
            let _ = app.emit("usticky://floating-bottomed", is_bounded);
        }
    }
    Ok(())
}

// ── i18n ──

#[tauri::command]
pub async fn get_app_locale(store: State<'_, SharedStore>) -> Result<String, String> {
    // store 持久化的 locale 是单一来源；rust-i18n 状态在 load_or_init 时
    // 已经同步过 set_locale，这里直接读 store（即便中途有别处改了
    // rust_i18n 状态，重启就同步回来）。
    Ok(store
        .read()
        .await
        .locale()
        .map(String::from)
        .unwrap_or_else(|| rust_i18n::locale().to_string()))
}

/// 切换 locale + 持久化 + 通知所有 webview（AGENTS.md #15）。
///
/// persist 失败时不静默吞：emit `usticky://persist-failed` 让前端 mini-flash
/// 提示用户（避免"切语言 → 重启 → 退回默认"的鬼故事）。persist 失败也
/// 不回滚内存态 —— 用户已经看到新语言了，再回滚体验更差；下次启动
/// 默认值是次要损害。
#[tauri::command]
pub async fn set_app_locale(
    app: AppHandle,
    store: State<'_, SharedStore>,
    locale: String,
) -> Result<(), String> {
    // **P2-3 / P2-20 fix**：locale whitelist。rust_i18n::set_locale 接受任意字符串
    // 但**不**校验是否在 `locales/*.json` 里 —— 错值会导致后续 t!() 调用返
    // 原 key 字符串（`"commands.error.empty_title"` 直接当 UI 文案），用户
    // 看到的是 dotted key 而不是翻译。先白名单再 set_locale。
    if !["en", "zh-CN"].contains(&locale.as_str()) {
        return Err(rust_i18n::t!("commands.error.unsupported_locale").to_string());
    }
    rust_i18n::set_locale(&locale);
    {
        let mut s = store.write().await;
        s.set_locale(locale.clone());
    }
    // **P1-2 fix**：走 persist_only helper，无 RwLock 跨 I/O。
    persist_only(&app, store.inner()).await;
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
    let parsed =
        PinMode::from_str_opt(mode).ok_or_else(|| format!("invalid pin mode: {}", mode))?;
    apply_pin_mode_to_window(app, parsed);
    {
        let mut s = store.write().await;
        s.set_pin_mode(parsed);
    }
    // **P1-2 fix**：走 persist_only helper，无 RwLock 跨 I/O。
    persist_only(app, store).await;
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

/// 切手型/箭头光标（前端 hover 命中操作按钮时调用）。
/// macOS 未聚焦窗口 WKWebView 不更新光标，靠 NSCursor 手动兜底；
/// Win/Linux 由平台层 no-op（详见 platform::set_cursor_pointer_shape）。
#[tauri::command]
pub fn set_cursor_pointer(app: AppHandle, pointer: bool) {
    crate::platform::set_cursor_pointer_shape(&app, pointer);
}

// ── Quick-add 快捷键 ──

/// 返回当前持久化的 quick-add 快捷键（accelerator 字符串，如 `"Cmd+Shift+Space"`）。
/// 没存过则返回平台默认（macOS = Cmd，其他 = Ctrl）。
#[tauri::command]
pub async fn get_quick_add_shortcut(store: State<'_, SharedStore>) -> Result<String, String> {
    Ok(store.read().await.quick_add_shortcut())
}

/// 设置并注册新的 quick-add 快捷键。
///
/// 流程：
///   1. 用 `parse_shortcut` 校验字符串能解析（不能解析返 Err）
///   2. 写 store + 持久化
///   3. 调 [`register_quick_add_shortcut`]（先 unregister_all 再注册新的）
///   4. emit `usticky://shortcut-changed` —— 浮窗 input hint + 设置面板 + tray
///      label 都听这个事件刷新
///
/// 校验失败时**不**写 store —— 防止坏值落盘导致下次启动快捷键失效。
#[tauri::command]
pub async fn set_quick_add_shortcut(
    app: AppHandle,
    store: State<'_, SharedStore>,
    accelerator: String,
) -> Result<(), String> {
    // 1. parse + **P1-5 fix**：modifier 必需。裸键（"F12"、"Space"）会被 OS
    //    直接吞掉或抢走其它 app 的焦点输入 —— 强制要求 CONTROL/ALT/META/SUPER
    //    至少一个。后端再校验一遍，防止前端 UI 没拦截（中间人篡改 IPC、未来
    //    加新入口忘记前端校验）。
    let parsed =
        crate::parse_shortcut(&accelerator).map_err(|e| format!("invalid shortcut: {e}"))?;
    use tauri_plugin_global_shortcut::Modifiers as Mods;
    let has_modifier = parsed
        .mods
        .intersects(Mods::CONTROL | Mods::ALT | Mods::META | Mods::SUPER);
    if !has_modifier {
        return Err(rust_i18n::t!("commands.error.shortcut_no_modifier").to_string());
    }

    // 2. **P1-4 fix**：snapshot 旧 accelerator —— persist 失败时回滚 in-memory
    //    + 不重新注册 OS 级快捷键，让用户保留旧可用快捷键。
    let previous = store.read().await.quick_add_shortcut();
    {
        let mut s = store.write().await;
        s.set_quick_add_shortcut(accelerator.clone());
    }

    // 3. **P1-2 fix**：裸 persist_only helper，零 RwLock 跨 I/O。
    let (path, data) = {
        let s = store.read().await;
        (s.data_path_clone(), s.data_clone())
    };
    if let Some(p) = path {
        if let Err(e) = crate::todo::persist_to_disk(&p, &data) {
            tracing::error!("set_quick_add_shortcut persist failed: {}", e);
            // 回滚 in-memory state 到 previous
            store.write().await.set_quick_add_shortcut(previous.clone());
            let _ = app.emit("usticky://persist-failed", e.to_string());
            // 不重新注册 —— store 还是 previous，OS 绑定也没动，行为一致
            return Err(format!("persist failed: {e}"));
        }
    }

    // 4. **P1-3 fix**：传 previous 给 register_quick_add_shortcut 作为回退，
    //    它的内部"parse → unregister_all → on_shortcut"流程在 on_shortcut
    //    失败时 best-effort 用同一 handler 装回 previous。
    crate::register_quick_add_shortcut(&app, store.inner(), Some(&previous));

    // 5. emit 同步给前端 / tray
    let _ = app.emit("usticky://shortcut-changed", accelerator);
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
