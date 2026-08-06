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

/// todo title 校验：trim + 非空 + ≤ 114514 字符(彩蛋上限)。
///
/// **P2-7 fix**：add_todo 已有这套校验，update_todo 之前**完全跳过**，
/// 用户（或前端 bug）可以把 title 改成空串 / 纯空格 / 超长串。
/// 抽到独立函数让 add/update 共用，避免校验规则分叉。
///
/// 114514 的来历：彩蛋。把 280 → 114514,纪念经典的「1919810」同款
/// 数字梗。原本上限来自早期推特 280 字符,视口窄 + 标题超长会强制换行
/// 破坏 hover-expand 手势;改大后基本可视为「不限制」,纯当彩蛋来玩。
fn validate_title(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim().to_string();
    if trimmed.is_empty() {
        return Err(rust_i18n::t!("commands.error.empty_title").into());
    }
    if trimmed.chars().count() > 114514 {
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
    // v0.2 撤销删除：附件文件**不**在这里删。前端把被删 Todo 存进 undo 栈，
    // 8s 内可点「撤销」调 `restore_todo` 恢复（图片完整可恢复）；超时后前端
    // 调 `purge_attachment` 真删文件。崩溃/异常退出留下的孤儿由启动孤儿扫描
    // 兜底（`Store::purge_orphan_attachments`）。
    // emit 被删的完整 Todo -- 主浮窗据此显示「撤销」action flash。统一走
    // 事件（而非由各调用点自己管 undo）让预览窗删除也有撤销入口。
    let _ = app.emit("usticky://todo-deleted", &deleted);
    persist_and_emit(&app, &store).await;
    Ok(deleted)
}

/// 撤销删除 - 把前端 undo 栈暂存的完整 Todo 塞回 store。
///
/// 复用原 id/order/status，尽量回到原位（见 `Store::restore` 的退化说明）。
///
/// **P0-1 fix**：persist 失败回滚 in-memory restore。restore 是"撤销删除"，
/// 若 persist 失败而内存留着这条 todo，下次启动 todos.json 没有它 ->
/// `purge_orphan_attachments` 会把它的附件文件当孤儿删掉 -> 用户的撤销
/// （含图片）不可逆丢失。回滚内存态让 in-memory 跟磁盘一致（都"未恢复"），
/// 附件留作孤儿由下次启动扫描清（诚实失败，而非假成功 + 重启后图片没了）。
/// 抄 [`set_quick_add_shortcut`] 的 snapshot + rollback 模板。
#[tauri::command]
pub async fn restore_todo(
    app: AppHandle,
    store: State<'_, SharedStore>,
    todo: Todo,
) -> Result<Todo, String> {
    let restored_id = todo.id.clone();
    // restore 返回 Some = 实际插入了；None = id 已存在（重复 restore / 并发）
    let inserted = {
        let mut s = store.write().await;
        s.restore(todo)
    };
    let restored = match inserted {
        Some(t) => t,
        None => {
            // id 已存在 -> 无需 persist / emit，直接返当前那条
            let existing = store
                .read()
                .await
                .todos()
                .iter()
                .find(|t| t.id == restored_id)
                .cloned()
                .ok_or_else(|| rust_i18n::t!("commands.error.not_found").to_string())?;
            return Ok(existing);
        }
    };
    // 实际插入了 -> persist。失败则回滚（删掉刚插入的，让内存跟磁盘一致）。
    let (snap, data, path) = {
        let s = store.read().await;
        (s.snapshot(), s.data_clone(), s.data_path_clone())
    };
    let persist_err: Option<String> = match path {
        Some(p) => crate::todo::persist_to_disk(&p, &data)
            .err()
            .map(|e| e.to_string()),
        None => Some("data path not initialized".to_string()),
    };
    if let Some(e) = persist_err {
        tracing::error!("restore_todo persist failed: {}", e);
        let _ = app.emit("usticky://persist-failed", e.clone());
        // 回滚 in-memory：删掉刚 restore 进去的 todo
        store.write().await.delete(&restored_id);
        // 不 emit todos-changed：内存已回到"未恢复"，前端拿 Err 自己处理 flash
        return Err(format!("restore failed: persist error: {e}"));
    }
    emit_todos_changed(&app, &snap);
    Ok(restored)
}

/// 真删单个附件文件 - 前端 undo 栈超时（用户没点撤销）后调用。
///
/// 安全校验：`file` 必须是纯文件名，禁止含路径分隔符 / `..` / `:` / NUL，
/// 否则前端（或中间人篡改 IPC）可构造 `../../etc/passwd` 删任意文件。不 emit
/// todos-changed、不 persist - 这个调用只动磁盘文件，不改 todo 数据。
///
/// **P1-1 fix**：补 `:` 和 `\0` 拦截。旧实现只拦 `/ \ .. 空`，`C:foo`
/// 这类 Windows drive-relative 路径能逃出 attachments 目录（`dir.join("C:foo")`
/// 在 Windows 上是 drive-relative，`remove_file` 命中 C: 盘相对 CWD 的文件）。
/// 附件文件名恒为 `<uuid>.<ext>`，不含冒号，拒冒号零误伤。NUL 字节 Rust std
/// 本就拒（`OsStr::to_cstring` 报错），这里兜底防御。
#[tauri::command]
pub async fn purge_attachment(store: State<'_, SharedStore>, file: String) -> Result<(), String> {
    if file.is_empty()
        || file.contains('/')
        || file.contains('\\')
        || file.contains("..")
        || file.contains(':')
        || file.contains('\0')
    {
        return Err("invalid attachment file name".to_string());
    }
    let dir = store.read().await.attachments_dir();
    if let Some(dir) = dir {
        let path = dir.join(&file);
        if let Err(e) = std::fs::remove_file(&path) {
            // NotFound 不算错（已被启动孤儿扫描清掉 / 用户手动清过）
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!("purge attachment {:?} failed: {}", path, e);
            }
        }
    }
    Ok(())
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

// ── 剪贴板粘贴（v0.2）──

/// 粘贴结果分类 —— 前端据此选 mini-flash 文案。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PasteOutcome {
    pub kind: String, // "text" | "image"
    pub title: String,
}

/// 附件目录绝对路径（前端 convertFileSrc 拼缩略图 / 预览图 URL 用）。
/// 顺带 create_dir_all —— 首次粘贴前目录可能不存在。
#[tauri::command]
pub async fn get_attachments_dir(store: State<'_, SharedStore>) -> Result<String, String> {
    let dir = store
        .read()
        .await
        .attachments_dir()
        .ok_or_else(|| rust_i18n::t!("commands.error.no_attachments_dir").to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("create attachments dir: {e}"))?;
    Ok(dir.to_string_lossy().into_owned())
}

/// 读取剪贴板并创建 pending todo（粘贴按钮 + 输入框 Cmd+V 共同入口）。
///
///   - 剪贴板是图片 → 落盘 `attachments/<uuid>.<ext>` + 创建带 attachment 的 todo
///   - 剪贴板是文本 → 整段文本作为一个 todo（多行保留，预览窗口负责展示全文）
///   - 空剪贴板 → Err("empty")，前端 flash 提示
///
/// 图片落盘失败（磁盘满 / 权限）→ Err 上抛，**不**创建半成品 todo。
///
/// `title`：输入框 Cmd+V 粘图片时把已键入的文字作为图片 todo 的标题传进来
/// （文字被消费进 todo，前端随后清空输入框）。粘贴按钮 / 输入框无文字时传 None，
/// 回退到 img.name（文件名）；都无则空标题（纯图 todo，前端整宽显示）。文本分支忽略此参数。
#[tauri::command]
pub async fn paste_from_clipboard(
    app: AppHandle,
    store: State<'_, SharedStore>,
    title: Option<String>,
) -> Result<PasteOutcome, String> {
    match crate::clipboard::read(&app) {
        crate::clipboard::ClipboardContent::Text(text) => {
            let title = validate_title(&text)?;
            let todo = {
                let mut s = store.write().await;
                s.add(title.clone())
            };
            persist_and_emit(&app, &store).await;
            Ok(PasteOutcome {
                kind: "text".into(),
                title: todo.title,
            })
        }
        crate::clipboard::ClipboardContent::Image(img) => {
            let dir = store
                .read()
                .await
                .attachments_dir()
                .ok_or_else(|| rust_i18n::t!("commands.error.no_attachments_dir").to_string())?;
            std::fs::create_dir_all(&dir).map_err(|e| format!("create attachments dir: {e}"))?;
            let file = format!("{}.{}", uuid::Uuid::new_v4(), img.ext);
            let path = dir.join(&file);
            // **P1 fix（review v0.2.6）**：attachment 落盘 mode 0o600，对齐 todos.json
            // 的安全姿态。std::fs::write 走默认 mode (0o666 & ~umask，常见 0o644)，
            // 剪贴板图片常常含敏感内容（聊天截图、密码管理器 UI、扫描件），
            // 同机其他账号可读。todo.rs persist_to_disk 已用同模式 (v0.1.5 P2-1)。
            //
            // **P2-5 fix**：write_all 失败（磁盘满 / 权限被剥中途）时清掉
            // 0 字节 / 半截孤儿文件。不清理也能被下次启动 purge_orphan_attachments
            // 扫掉，但立即清理更干净（不依赖启动兜底，避免瞬态孤儿在 attachments/
            // 目录里留着）。
            #[cfg(unix)]
            {
                use std::io::Write;
                use std::os::unix::fs::OpenOptionsExt;
                std::fs::OpenOptions::new()
                    .create(true)
                    .truncate(true)
                    .write(true)
                    .mode(0o600)
                    .open(&path)
                    .and_then(|mut f| f.write_all(&img.data))
                    .map_err(|e| {
                        let _ = std::fs::remove_file(&path);
                        format!("write attachment: {e}")
                    })?;
            }
            #[cfg(not(unix))]
            std::fs::write(&path, &img.data).map_err(|e| {
                let _ = std::fs::remove_file(&path);
                format!("write attachment: {e}")
            })?;

            let title = match title.as_deref().map(str::trim) {
                Some(n) if !n.is_empty() => validate_title(n)?,
                _ => match img.name.as_deref().map(str::trim) {
                    Some(n) if !n.is_empty() => validate_title(n)?,
                    // 纯图 todo：无标题。前端 buildTodoRow 见空标题让 .todo-title
                    // 折叠（:empty），图片独占整宽（v0.2.6 inline 图片 1:1 布局）。
                    _ => String::new(),
                },
            };
            let attachment = crate::todo::TodoAttachment {
                file,
                mime: img.mime.to_string(),
                width: img.width,
                height: img.height,
            };
            let todo = {
                let mut s = store.write().await;
                s.add_with_attachment(title.clone(), Some(attachment))
            };
            persist_and_emit(&app, &store).await;
            Ok(PasteOutcome {
                kind: "image".into(),
                title: todo.title,
            })
        }
        crate::clipboard::ClipboardContent::Empty => Err("empty".into()),
    }
}

// ── QuickLook 式预览窗口（v0.2）──

/// v0.2.1 prewarm 竞态防线：预览窗已建但 webview 还没加载完时，
/// open_preview_window 的 `usticky://preview-todo` emit 会丢（listener
/// 还没注册）。reuse 路径先把 id 存这里，preview.ts init 末尾主动
/// `take_pending_preview_todo` 取走 —— emit 丢了也能自愈。
static PENDING_PREVIEW_TODO: std::sync::OnceLock<std::sync::Mutex<Option<String>>> =
    std::sync::OnceLock::new();

fn pending_preview_todo() -> &'static std::sync::Mutex<Option<String>> {
    PENDING_PREVIEW_TODO.get_or_init(|| std::sync::Mutex::new(None))
}

#[tauri::command]
pub async fn take_pending_preview_todo() -> Result<Option<String>, String> {
    Ok(pending_preview_todo()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take())
}

/// hover 预览的 pin 按钮 -> 把当前 todo 提升为**独立固定窗**
/// （label=`preview-pin-<todoId>`，URL `?pinned=1`）。固定窗常驻屏幕，blur /
/// 浮窗 hide 都不自关，只走 Esc / 取消固定按钮（preview.ts closeSelf）。
/// 可同时存在多个（每个 todo 一个）。
///
/// 沿用当前 hover 预览窗（`preview`）的位置 / 尺寸创建固定窗，平滑过渡；
/// 然后关掉 hover 预览。dedup：该 todo 已有固定窗 -> focus 它，不开新的。
#[tauri::command]
pub async fn pin_preview(app: AppHandle, todo_id: String) -> Result<(), String> {
    let label = format!("preview-pin-{}", todo_id);
    // dedup：已有该 todo 的固定窗 -> focus 它，关掉 hover 预览即可。
    if let Some(w) = app.get_webview_window(&label) {
        let _ = w.show();
        let _ = w.set_focus();
        close_hover_preview_for_pin(&app);
        return Ok(());
    }
    // 沿用 hover 预览窗（如在屏）的位置 / 尺寸 -> 固定窗原地上屏，无跳变。
    // pin 按钮只在 hover 预览里，理论上 `preview` 一定开着；兜底用浮窗左侧。
    let scale = app
        .get_webview_window("preview")
        .and_then(|p| p.scale_factor().ok())
        .or_else(|| {
            app.get_webview_window("floating")
                .and_then(|f| f.scale_factor().ok())
        })
        .unwrap_or(1.0);
    let (pos_x_logical, pos_y_logical, w_logical, h_logical) =
        if let Some(p) = app.get_webview_window("preview") {
            let pos = p.outer_position().map_err(|e| e.to_string())?;
            let size = p.outer_size().map_err(|e| e.to_string())?;
            (
                pos.x as f64 / scale,
                pos.y as f64 / scale,
                size.width as f64 / scale,
                size.height as f64 / scale,
            )
        } else {
            // 兜底：浮窗左侧 12px GAP，尺寸用默认（极少走到）。
            let (w_l, h_l) = (460.0, 340.0);
            let fpos = app
                .get_webview_window("floating")
                .and_then(|f| f.outer_position().ok())
                .unwrap_or(tauri::PhysicalPosition::new(0, 0));
            (
                fpos.x as f64 / scale - w_l - 12.0,
                fpos.y as f64 / scale,
                w_l,
                h_l,
            )
        };

    let title = rust_i18n::t!("window.preview").to_string();
    let url = WebviewUrl::App(format!("preview.html?id={}&pinned=1", todo_id).into());
    let _win = WebviewWindowBuilder::new(&app, &label, url)
        .title(title)
        .inner_size(w_logical, h_logical)
        .position(pos_x_logical, pos_y_logical)
        .resizable(true)
        .decorations(false)
        .transparent(true)
        // 同 hover 预览：去原生阴影（黑边），投影交 preview.css --pv-shadow。
        .shadow(false)
        .always_on_top(true)
        .accept_first_mouse(true)
        .focused(true)
        .visible(true)
        .build()
        .map_err(|e| format!("create pinned preview window: {e}"))?;
    // 新 always-on-top 窗口上屏触发浮窗合成层重排 -> 刷玻璃。
    let _ = app.emit("usticky://backdrop-refresh", ());
    // 关掉 hover 预览（内容已转到固定窗）。beforeunload 可能不触发，
    // 直接 emit preview-closed 让浮窗状态机复位。
    close_hover_preview_for_pin(&app);
    Ok(())
}

/// pin_preview 内部：关掉 hover 预览窗（label="preview"）并广播 preview-closed
/// 让浮窗状态机复位。固定窗（label="preview-pin-*"）不归本函数管 -- 独立常驻。
fn close_hover_preview_for_pin(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("preview") {
        let _ = w.close();
    }
    let _ = app.emit("usticky://preview-closed", ());
    let _ = app.emit("usticky://backdrop-refresh", ());
}

/// 预热：首次 hover 浮窗时**隐藏**创建预览窗，webview 加载开销提前付掉，
/// 第一次 dwell 打开不再等 300-500ms 白屏。已存在则 no-op。
/// 隐藏窗口不抢焦点、不上屏（visible(false)），open_preview_window 复用时
/// show + 归位。
#[tauri::command]
pub async fn prewarm_preview_window(app: AppHandle) -> Result<(), String> {
    if app.get_webview_window("preview").is_some() {
        return Ok(());
    }
    let title = rust_i18n::t!("window.preview").to_string();
    let _win = WebviewWindowBuilder::new(&app, "preview", WebviewUrl::App("preview.html".into()))
        .title(title)
        .inner_size(460.0, 340.0)
        .resizable(true)
        .decorations(false)
        .transparent(true)
        // v0.2.3：去原生窗口阴影 —— 透明无边框窗的 NSWindow shadow 会紧贴
        // panel 外形画一圈硬黑边（用户实测"太丑"）。投影由 preview.css 的
        // --pv-shadow（柔和 50px blur）负责。
        .shadow(false)
        .always_on_top(true)
        .accept_first_mouse(true)
        .focused(false)
        .visible(false)
        .build()
        .map_err(|e| format!("prewarm preview window: {e}"))?;
    Ok(())
}

/// 预览窗口逻辑尺寸：
/// - 文本卡：宽 460；高 = 前端预测量内容高（main.ts measurer，与
///   .preview-text 同宽同字体）+ CHROME_H（app padding 6×2 + panel
///   padding 14×2 + gap 10 + footer ~24（v0.2.4 起含日期+按钮，比纯
///   hint 行高 ~8px）≈ 74），clamp 130-720。未带测量值（竞态 / 老前端）
///   兜底 340。**一次开到位 —— show 后再 resize 是预览窗闪烁的根源之一**。
/// - 图片卡：按附件宽高比缩放（宽 240-640，下方留 ~178px caption+footer
///   编辑区），高 clamp 200-720。
fn preview_logical_size(todo: &Todo, text_h: Option<f64>) -> (f64, f64) {
    const TEXT_W: f64 = 460.0;
    const TEXT_FALLBACK_H: f64 = 340.0;
    const CHROME_H: f64 = 74.0;
    const CAPTION_AREA_H: f64 = 178.0;
    match &todo.attachment {
        Some(att) => {
            let (aw, ah) = match (att.width, att.height) {
                (Some(w), Some(h)) if w > 0 && h > 0 => (w as f64, h as f64),
                _ => return (TEXT_W, TEXT_FALLBACK_H),
            };
            let w = aw.clamp(240.0, 640.0);
            let img_h = w * ah / aw;
            let h = (img_h + CAPTION_AREA_H).clamp(200.0, 720.0);
            (w, h)
        }
        None => {
            let h = text_h.map_or(TEXT_FALLBACK_H, |t| (t + CHROME_H).clamp(130.0, 720.0));
            (TEXT_W, h)
        }
    }
}

/// 跟手定位（v0.2.1+）：**默认浮窗左边、y 对齐被 hover 卡片的屏幕顶**
/// （`anchor_sy`，物理像素）；左边空间不足 → 右边；两边都放不下 → 右侧
/// clamp 兜底（允许重叠 —— hover emitter 把预览窗算作 inside，重叠不会
/// 再触发 unhover→close 循环）。y clamp 进显示器边界。
///
/// 坐标系：tao `outer_position`/`outer_size`/`monitor` 全是**物理像素**、
/// top-left origin（y 向下）。
#[allow(clippy::too_many_arguments)]
fn preview_position(
    fx: i32,
    fw: i32,
    pw: i32,
    ph: i32,
    mx: i32,
    my: i32,
    mw: i32,
    mh: i32,
    anchor_sy: i32,
) -> (i32, i32) {
    const GAP: i32 = 12;
    let y = anchor_sy.clamp(my, (my + mh - ph).max(my));
    // 左（默认）
    let left_x = fx - pw - GAP;
    if left_x >= mx {
        return (left_x, y);
    }
    // 右（左边空间不足）
    let right_x = fx + fw + GAP;
    if right_x + pw <= mx + mw {
        return (right_x, y);
    }
    // 兜底：右侧 clamp 进显示器（可能重叠浮窗，由 over_preview-inside 兜底）
    (right_x.clamp(mx, (mx + mw - pw).max(mx)), y)
}

/// 打开 / 复用预览窗口。
///
///   - `pinned = false`：hover 触发的非聚焦预览（QuickLook 面板语义，
///     不抢焦点，鼠标离开后由浮窗前端发起关闭）。
///   - `pinned = true`：用户点击缩略图 / 卡片触发的聚焦预览（编辑态入口）。
///   - `anchor_y`：被 hover 卡片的**视口相对**顶边（逻辑 px，前端
///     `getBoundingClientRect().top`）。换算成屏幕物理坐标后做跟手
///     定位（预览顶对齐卡片顶）；缺省 = 浮窗顶。
///   - `text_h`：前端预测量的文本内容高（逻辑 px），文本卡一次开到位，
///     避免 show 后再 resize 闪烁。
///
/// 窗口已存在时**不重建**：更新 URL query 会让 webview reload 丢编辑内容，
/// 改为 emit `usticky://preview-todo` 让 preview.ts 原地换内容 + resize/移窗。
/// emit 前先存 PENDING_PREVIEW_TODO —— webview 未加载完时 emit 会丢，
/// preview.ts init 末尾主动取（prewarm 竞态防线）。
///
/// backdrop-refresh 只在**隐藏→上屏**（prewarm 首显 / 全新创建）和关闭时
/// emit：新 always-on-top 窗口上屏/离屏会触发 macOS 合成层重排，浮窗
/// WKWebView backdrop sample 随之失效。**可见窗口的换内容/跟手移动不刷**
/// —— 每次 hover 换卡都刷会让浮窗整窗 repaint，那才是用户看到的"闪"。
#[tauri::command]
pub async fn open_preview_window(
    app: AppHandle,
    store: State<'_, SharedStore>,
    todo_id: String,
    pinned: bool,
    anchor_y: Option<f64>,
    text_h: Option<f64>,
) -> Result<(), String> {
    // 已有该 todo 的独立固定窗 -> 不开 hover 预览（避免同 todo 两个窗口）。
    // 固定窗已可见，用户看那个即可；hover 该 todo 也不重复弹。
    let pin_label = format!("preview-pin-{}", todo_id);
    if app.get_webview_window(&pin_label).is_some() {
        return Ok(());
    }
    let todo = store
        .read()
        .await
        .todos()
        .iter()
        .find(|t| t.id == todo_id)
        .cloned()
        .ok_or_else(|| rust_i18n::t!("commands.error.not_found").to_string())?;

    let (w_logical, h_logical) = preview_logical_size(&todo, text_h);

    // outer_position/outer_size 是物理像素，inner_size 是逻辑像素 —— 用
    // scale_factor 换算，Retina 上否则窗口只有预期一半大（同 resize_floating_window）。
    let (pos_x, pos_y, w_phys, h_phys, scale) = {
        let fw = app
            .get_webview_window("floating")
            .ok_or_else(|| "floating window missing".to_string())?;
        let scale = fw.scale_factor().unwrap_or(1.0);
        let fpos = fw.outer_position().map_err(|e| e.to_string())?;
        let fsize = fw.outer_size().map_err(|e| e.to_string())?;
        let w_phys = (w_logical * scale).round() as i32;
        let h_phys = (h_logical * scale).round() as i32;
        let mon = fw.current_monitor().ok().flatten();
        // **v0.2.4 Dock 修复**：用 work_area（macOS visible frame —— 扣除
        // 菜单栏 + Dock 占用区）而不是 monitor.size()（全屏物理分辨率）。
        // 用全屏高 clamp 时，贴底 hover 长卡的预览窗底边会滑进 Dock 下面
        // 被遮挡（用户截图实证）。Win 上 work_area 同样扣任务栏。
        let (mx, my, mw, mh) = mon
            .map(|m| {
                let wa = m.work_area();
                (
                    wa.position.x,
                    wa.position.y,
                    wa.size.width as i32,
                    wa.size.height as i32,
                )
            })
            .unwrap_or((0, 0, 1920, 1080));
        // 卡片视口顶（逻辑）→ 屏幕物理 y：浮窗物理顶 + anchor_y × scale。
        // 无边框窗口内容原点 ≈ 窗口原点（无 titlebar 偏移）。
        let anchor_sy = anchor_y
            .map(|ay| fpos.y + (ay * scale).round() as i32)
            .unwrap_or(fpos.y);
        let (x, y) = preview_position(
            fpos.x,
            fsize.width as i32,
            w_phys,
            h_phys,
            mx,
            my,
            mw,
            mh,
            anchor_sy,
        );
        (x, y, w_phys, h_phys, scale)
    };

    if let Some(w) = app.get_webview_window("preview") {
        // 复用：换内容 + 跟手归位。pinned 才抢焦点（hover 预览不抢）。
        let was_visible = w.is_visible().unwrap_or(true);
        let _ = w.set_size(tauri::PhysicalSize::new(w_phys as u32, h_phys as u32));
        let _ = w.set_position(tauri::PhysicalPosition::new(pos_x, pos_y));
        if pinned {
            let _ = w.show();
            let _ = w.set_focus();
        } else {
            // hover 面板：show 不抢 key（macOS 上 tao show() 底层
            // makeKeyAndOrderFront 会偷焦点 → WKWebView 恢复 mouseenter
            // → preview-entered 误 pin 锁死 hover 换卡，v0.2.3 bug）
            crate::platform::show_window_no_activate(&app, "preview");
        }
        {
            let mut pending = pending_preview_todo().lock().unwrap_or_else(|e| e.into_inner());
            *pending = Some(todo_id.clone());
        }
        w.emit("usticky://preview-todo", serde_json::json!({ "id": todo_id }))
            .map_err(|e| e.to_string())?;
        // 只在隐藏→上屏时刷新浮窗玻璃（见 doc comment）
        if !was_visible {
            let _ = app.emit("usticky://backdrop-refresh", ());
        }
        return Ok(());
    }

    let title = rust_i18n::t!("window.preview").to_string();
    let url = WebviewUrl::App(format!("preview.html?id={}", todo_id).into());
    let _win = WebviewWindowBuilder::new(&app, "preview", url)
        .title(title)
        .inner_size(w_logical, h_logical)
        // builder position 是逻辑坐标（x/y 两个 f64），pos_x/pos_y 算的是
        // 物理像素 —— 除 scale 换回逻辑，Retina 上否则偏一倍。
        .position(pos_x as f64 / scale, pos_y as f64 / scale)
        .resizable(true)
        .decorations(false)
        .transparent(true)
        // v0.2.3：去原生窗口阴影 —— 透明无边框窗的 NSWindow shadow 会紧贴
        // panel 外形画一圈硬黑边（用户实测"太丑"）。投影由 preview.css 的
        // --pv-shadow（柔和 50px blur）负责。
        .shadow(false)
        .always_on_top(true)
        .accept_first_mouse(true)
        .focused(pinned)
        .visible(true)
        .build()
        .map_err(|e| format!("create preview window: {e}"))?;
    let _ = app.emit("usticky://backdrop-refresh", ());
    Ok(())
}

/// 关闭预览窗口（浮窗 hover 收尾 / preview.ts 自关 / 浮窗 hide 兜底共用）。
/// 幂等：窗口不存在时静默成功。
///
/// `force = false`（默认）：窗口**聚焦中**（用户在预览里编辑）时跳过 ——
/// 浮窗 hover(false) / grace close 不该打断编辑；编辑态窗口由 preview.ts
/// 自己的 blur/Esc 关闭。`force = true`：浮窗 hide 等兜底路径强制关。
#[tauri::command]
pub async fn close_preview_window(app: AppHandle, force: Option<bool>) -> Result<(), String> {
    if let Some(w) = app.get_webview_window("preview") {
        if !force.unwrap_or(false) && w.is_focused().unwrap_or(false) {
            return Ok(());
        }
        w.close().map_err(|e| e.to_string())?;
        // 关窗同样触发合成层重排 → 浮窗玻璃重采样（见 open 的注释）
        let _ = app.emit("usticky://backdrop-refresh", ());
        // 兜底直接 emit preview-closed：w.close() 在 Tauri 2 / WKWebView 下
        // 可能不触发 webview beforeunload，preview.ts 的 beforeunload 监听不来，
        // 浮窗收不到 preview-closed，穿缝期间保留的强调无法释放（卡死，只有
        // 点别的应用触发 onFocusChanged 才摘）。preview.ts beforeunload 仍会
        // 再 emit 一次，listener 幂等无副作用。
        let _ = app.emit("usticky://preview-closed", ());
    }
    Ok(())
}
