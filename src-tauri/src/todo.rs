// Todo 数据模型 + JSON 持久化
//
// 设计要点（沿用 Musage 的 keys.json 经验）：
//   - 单文件 JSON，路径 = dirs::data_dir() / "usticky" / "todos.json"
//   - 原子写：write to tmp + rename（避免崩溃中途留下半截文件）
//   - Unix 0600 权限（其它用户不能读你的 todo）
//   - 解析失败 → backup 到 todos.json.bak.<ts>，用空 store 顶上
//   - 内存态在 Store 里，IPC 走 &SharedStore (Arc<RwLock<Store>>)
//
// 不需要 polling / backoff —— todo 是被动存储，事件驱动。
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tauri::AppHandle;
use tauri::Manager;

// rust_i18n::i18n!("locales") 在 lib.rs 顶部 crate 级初始化，此处不需要再调。

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TodoStatus {
    Pending,
    Done,
}

/// 浮窗层级模式 —— 跟 Musage 同款三档：
/// - PinTop: 始终置顶（kCGFloatingWindowLevel / HWND_TOPMOST）
/// - PinBottom: 默认置底（kCGNormalWindowLevel - 1 / HWND_BOTTOM），
///              鼠标 hover 时临时置顶
/// - Normal: 不强制层级，跟普通窗口一样
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PinMode {
    PinTop,
    PinBottom,
    Normal,
}

impl Default for PinMode {
    fn default() -> Self {
        Self::PinBottom  // v0.1.2 默认置底（hover 时临时置顶，不挡其他 app）
    }
}

impl PinMode {
    /// 解析前端传过来的字符串。失败返 None。
    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s {
            "pin_top" => Some(Self::PinTop),
            "pin_bottom" => Some(Self::PinBottom),
            "normal" => Some(Self::Normal),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TodoPriority {
    P0,
    P1,
    P2,
    P3,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Todo {
    pub id: String,                  // UUID v4
    pub title: String,
    pub status: TodoStatus,
    pub priority: TodoPriority,
    pub created_at: i64,
    pub updated_at: i64,
    pub due_at: Option<i64>,
    pub tags: Vec<String>,
    pub order: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WindowGeom {
    pub x: Option<i32>,
    pub y: Option<i32>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

/// 存储结构（顶层 JSON）。
///
/// `todos` 是平铺数组 —— Usticky 不分层不分项目，简单就是好。
/// `window_geom` 单独存（避免 todos 的 update 触发不必要的窗口几何 persist）。
/// `pin_mode` 跨重启保留 —— PinBottom 用户一般不会反复切，存盘一次保终身。
/// `quick_add_shortcut` 跨重启保留 —— 用户改完后希望下次启动仍是自己设的键。
/// 默认值见 [`Store::quick_add_shortcut`]（macOS = `Cmd+Shift+Space`，
/// 其他平台 = `Ctrl+Shift+Space`），用 global-hotkey 的 `CmdOrCtrl` 关键字
/// 也可以让平台分支在 parse 时自动处理。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StoreData {
    pub todos: Vec<Todo>,
    pub window_geom: WindowGeom,
    pub pin_mode: Option<PinMode>,
    pub quick_add_shortcut: Option<String>,
}

/// Store —— 内存态 + 文件路径。
///
/// Mutex 保护 `data_path`（首次 load 后就 stable，理论上不需要 Mutex，
/// 但留着方便以后切 sqlite 时的 connection pool）。
///
/// `persist_lock` 串行化磁盘 I/O：
///   拖窗时 WindowEvent::Moved/Resized 在 macOS 上以 ~60Hz 派发，每个事件
///   spawn 一个新 task 调 `Store::persist`。多个 task 并发调 `persist_to_disk`
///   会同时打开同一个 `tmp` 文件 → 后到的 chmod/rename 失败（"atomic rename"
///   失败是因为前一个 rename 已经把 tmp 搬走了 / 目标已被替换）。`persist_lock`
///   保证同一时刻只有一个 task 走完"写 tmp + chmod + rename"全流程。
pub struct Store {
    data: StoreData,
    data_path: Mutex<Option<PathBuf>>,
    persist_lock: Mutex<()>,
}

impl Store {
    /// 加载或初始化 store。App 启动时调用一次。
    pub fn load_or_init(app: &AppHandle) -> Result<Self> {
        let data_path = resolve_data_path(app)?;
        let data = if data_path.exists() {
            match load_from_disk(&data_path) {
                Ok(d) => d,
                Err(e) => {
                    // 解析失败 → backup 后用空 store 顶上，不阻塞启动
                    tracing::warn!("todos.json 解析失败 ({}), backup + 启动空 store", e);
                    backup_corrupt_file(&data_path)?;
                    StoreData::default()
                }
            }
        } else {
            // 首次启动：确保目录存在
            if let Some(parent) = data_path.parent() {
                fs::create_dir_all(parent).context("create data dir")?;
            }
            StoreData::default()
        };
        Ok(Self {
            data,
            data_path: Mutex::new(Some(data_path)),
            persist_lock: Mutex::new(()),
        })
    }

    pub fn todos(&self) -> &[Todo] {
        &self.data.todos
    }

    pub fn todos_sorted(&self, status: TodoStatus) -> Vec<Todo> {
        let mut v: Vec<Todo> = self.data.todos.iter()
            .filter(|t| t.status == status)
            .cloned()
            .collect();
        v.sort_by_key(|t| t.order);
        v
    }

    pub fn add(&mut self, title: String) -> Todo {
        let now = chrono::Utc::now().timestamp_millis();
        let max_order = self.data.todos.iter()
            .filter(|t| t.status == TodoStatus::Pending)
            .map(|t| t.order)
            .max()
            .unwrap_or(-1);
        let todo = Todo {
            id: uuid::Uuid::new_v4().to_string(),
            title,
            status: TodoStatus::Pending,
            priority: TodoPriority::P2,
            created_at: now,
            updated_at: now,
            due_at: None,
            tags: vec![],
            order: max_order + 1,
        };
        self.data.todos.push(todo.clone());
        todo
    }

    pub fn update(&mut self, id: &str, title: Option<String>, status: Option<TodoStatus>) -> Option<Todo> {
        let now = chrono::Utc::now().timestamp_millis();
        let todo = self.data.todos.iter_mut().find(|t| t.id == id)?;
        if let Some(title) = title { todo.title = title; }
        if let Some(status) = status { todo.status = status; }
        todo.updated_at = now;
        Some(todo.clone())
    }

    pub fn delete(&mut self, id: &str) -> Option<Todo> {
        let idx = self.data.todos.iter().position(|t| t.id == id)?;
        Some(self.data.todos.remove(idx))
    }

    /// 拖拽后批量重排（按 status 内顺序）。
    ///
    /// `ids` 是 section 局部的新顺序（IPC caller 即前端 SortableJS
    /// `onEnd` 给的 DOM 顺序），仅包含被拖拽 section 的 todos。
    ///
    /// 实现要点（修复 v0.1.x 拖了无反应的 bug）：
    ///   1. **物理重排** `self.data.todos` —— 按 `ids` 的新顺序替换原 section
    ///      在 Vec 里的位置（其他 status 的 todo 完全保留原位置）。
    ///      只改 `t.order` 而不挪 Vec 是不行的，前端 `render` 用 `.filter()`
    ///      取的是 Vec 的数组顺序而不是按 order 排序，拖了看不到效果。
    ///   2. 顺带把 `t.order` 写成 section 局部索引（0,1,2,...，仅在被拖
    ///      section 内），跟 `add()` 的 `max_order + 1` 保持一致 —— 不被
    ///      改动的 todo `order` 保持原值，跨重启仍能正确还原。
    ///   3. 只在 `t.order` 变化时刷新 updated_at —— 拖了但 ids 顺序没变
    ///      时不应产生"更新时间"噪声（前版注释意图）。
    pub fn reorder(&mut self, ids: &[String]) {
        if ids.is_empty() {
            return;
        }

        // 1. 找被拖 section 在 Vec 里的最早位置 —— 作为新顺序的锚点。
        //    不依赖"section 在 Vec 里连续"的假设：防御 add/update 之后
        //    pending/done 在数组里穿插（虽然 add() 总 append，但 v0.2
        //    之后可能改）。
        let id_set: std::collections::HashSet<&str> = ids.iter().map(|s| s.as_str()).collect();
        let base_idx = match self.data.todos.iter().position(|t| id_set.contains(t.id.as_str())) {
            Some(i) => i,
            None => return,  // ids 全部找不到 —— 防御，不动 store
        };

        // 2. 把"被拖集合"按 ids 新顺序抽出来。若某 id 在 ids 里但
        //    self.data.todos 找不到（防御），整批跳过 —— 不让 store
        //    进入不一致状态。
        let mut moved: Vec<Todo> = Vec::with_capacity(ids.len());
        for id in ids {
            match self.data.todos.iter().find(|t| &t.id == id) {
                Some(t) => moved.push(t.clone()),
                None => return,
            }
        }

        // 3. 重建 Vec：base_idx 之前的不动 todo 原样搬过去，到 base_idx
        //    位置把整段 moved 写进去（同时刷 section-local order），再
        //    续接剩余不动 todo。moved 在循环里 consume 一次。
        let mut new_todos: Vec<Todo> = Vec::with_capacity(self.data.todos.len());
        let now = chrono::Utc::now().timestamp_millis();
        let mut moved_drained = false;
        let mut moved_iter = moved.into_iter();
        for (i, todo) in self.data.todos.drain(..).enumerate() {
            if id_set.contains(todo.id.as_str()) {
                // 原位置属于"被拖集合" —— 不写回，等会儿由 moved_iter 占据
                continue;
            }
            if !moved_drained && i >= base_idx {
                // 这是 base_idx 位置或之后的第一个不动 todo —— 在它前面
                // 灌入整段 moved，每条写一个 section-local order。
                for (j, mut m) in (&mut moved_iter).enumerate() {
                    let new_order = j as i32;
                    if m.order != new_order {
                        m.order = new_order;
                        m.updated_at = now;
                    }
                    new_todos.push(m);
                }
                moved_drained = true;
            }
            new_todos.push(todo);
        }
        // 防御：万一 moved_drained 没触发（base_idx 之后所有 todo 都
        // 是 moved，整个拖拽后没有"空位锚"），把剩余 moved 接到尾巴。
        for (j, mut m) in moved_iter.enumerate() {
            let new_order = j as i32;
            if m.order != new_order {
                m.order = new_order;
                m.updated_at = now;
            }
            new_todos.push(m);
        }

        self.data.todos = new_todos;
    }

    pub fn last_window_geom(&self) -> &WindowGeom {
        &self.data.window_geom
    }

    pub fn update_window_pos(&mut self, x: Option<i32>, y: Option<i32>) {
        if let Some(x) = x { self.data.window_geom.x = Some(x); }
        if let Some(y) = y { self.data.window_geom.y = Some(y); }
    }

    pub fn update_window_size(&mut self, w: Option<u32>, h: Option<u32>) {
        if let Some(w) = w { self.data.window_geom.width = Some(w); }
        if let Some(h) = h { self.data.window_geom.height = Some(h); }
    }

    pub fn pin_mode(&self) -> PinMode {
        self.data.pin_mode.unwrap_or_default()
    }

    pub fn set_pin_mode(&mut self, mode: PinMode) {
        self.data.pin_mode = Some(mode);
    }

    /// 当前快速唤出快捷键（accelerator 字符串，如 `"Cmd+Shift+Space"`）。
    /// 没存过就用平台默认（macOS = Cmd，其他 = Ctrl）。
    pub fn quick_add_shortcut(&self) -> String {
        self.data.quick_add_shortcut.clone()
            .unwrap_or_else(default_quick_add_shortcut)
    }

    pub fn set_quick_add_shortcut(&mut self, accelerator: String) {
        self.data.quick_add_shortcut = Some(accelerator);
    }
}

/// 平台默认快捷键。macOS 用 ⌘ Cmd，其他平台用 Ctrl —— 跟 AGENTS.md
/// 写的 `CmdOrCtrl+Shift+Space` 语义一致。
pub fn default_quick_add_shortcut() -> String {
    #[cfg(target_os = "macos")]
    { "Cmd+Shift+Space".to_string() }
    #[cfg(not(target_os = "macos"))]
    { "Ctrl+Shift+Space".to_string() }
}

/// AppConfig —— 跨重启保留的少量配置（目前只有 locale）。
///
/// 不并入 StoreData 是因为 locale 变更极频繁且不影响业务逻辑，独立出来
/// 减少 todos.json 的写放大。Musage 范式：cfg 存单独字段，必要时也走
/// AppConfig 单独持久化（v0.1 暂未 manage 到 Tauri state，因为 locale
/// 仅内存态够用；v0.2 加设置面板时再 app.manage(AppConfig::default())）
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub locale: String,
}

#[allow(dead_code)]
impl Default for AppConfig {
    fn default() -> Self {
        Self { locale: "zh-CN".to_string() }
    }
}

/// 轻量 snapshot —— emit 用，避免 IPC 传整个 Store。
#[derive(Debug, Clone, Serialize)]
pub struct TodoSnapshot {
    pub todos: Vec<Todo>,
    pub fetched_at: i64,
}

impl Store {
    /// 内存中 clone 一份快照（emit + IPC 返值都用这个）。
    pub fn snapshot(&self) -> TodoSnapshot {
        TodoSnapshot {
            todos: self.data.todos.clone(),
            fetched_at: chrono::Utc::now().timestamp_millis(),
        }
    }

    /// 持久化 + emit。
    ///
    /// 拿 `persist_lock` 串行化磁盘 I/O：拖窗时 Moved/Resized 事件 ~60Hz
    /// 派发，每个事件 spawn 出的 task 都进这里 —— 没锁就并发打开 tmp 文件，
    /// chmod 和 rename 互相覆盖，触发 "atomic rename failed" 噪声日志。
    ///
    /// poison 恢复（`unwrap_or_else(|e| e.into_inner())`）跟其他 Mutex 同款：
    /// 一旦 task panic 不让后续 persist 全部卡死。
    pub fn persist(&self, _app: &AppHandle) -> Result<()> {
        let _guard: MutexGuard<()> = self.persist_lock.lock()
            .unwrap_or_else(|e| e.into_inner());
        let path = self.data_path.lock().unwrap().clone()
            .context("data_path not initialized")?;
        persist_to_disk(&path, &self.data)
    }
}

fn resolve_data_path(app: &AppHandle) -> Result<PathBuf> {
    // 优先用 app 的 data_dir（macOS ~/Library/Application Support/<bundle id>，
    // Windows %APPDATA%/<bundle id>），找不到再 fallback 到 dirs::data_dir()
    if let Some(dir) = app.path().app_data_dir().ok() {
        Ok(dir.join("todos.json"))
    } else {
        let dir = dirs::data_dir().context("no data dir")?.join("usticky");
        Ok(dir.join("todos.json"))
    }
}

fn load_from_disk(path: &Path) -> Result<StoreData> {
    let bytes = fs::read(path).context("read todos.json")?;
    let data: StoreData = serde_json::from_slice(&bytes).context("parse todos.json")?;
    Ok(data)
}

fn persist_to_disk(path: &Path, data: &StoreData) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).context("create data dir")?;
    }
    let tmp = path.with_extension("json.tmp");
    {
        let mut f = fs::File::create(&tmp).context("create tmp file")?;
        let json = serde_json::to_vec_pretty(data).context("serialize")?;
        f.write_all(&json).context("write tmp")?;
        f.sync_all().context("fsync tmp")?;

        // Unix: 设 0600 权限
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = fs::Permissions::from_mode(0o600);
            fs::set_permissions(&tmp, perms).context("chmod 0600")?;
        }
    }
    fs::rename(&tmp, path).context("atomic rename")?;
    Ok(())
}

fn backup_corrupt_file(path: &Path) -> Result<()> {
    let ts = chrono::Utc::now().timestamp();
    let backup = path.with_extension(format!("json.bak.{}", ts));
    fs::rename(path, backup).context("backup corrupt file")?;
    Ok(())
}

#[cfg(test)]
mod reorder_tests {
    use super::*;

    /// Build a minimal StoreData + Store bypassing `load_or_init` (which
    /// needs AppHandle for data dir). Tests run on the in-memory methods
    /// that reorder / add / etc. actually mutate.
    fn fresh_store(todos: Vec<Todo>) -> Store {
        Store {
            data: StoreData {
                todos,
                ..StoreData::default()
            },
            data_path: Mutex::new(None),
            persist_lock: Mutex::new(()),
        }
    }

    fn mk(id: &str, status: TodoStatus, order: i32) -> Todo {
        Todo {
            id: id.to_string(),
            title: id.to_string(),
            status,
            priority: TodoPriority::P2,
            created_at: 0,
            updated_at: 0,
            due_at: None,
            tags: vec![],
            order,
        }
    }

    fn ids(store: &Store) -> Vec<String> {
        store.data.todos.iter().map(|t| t.id.clone()).collect()
    }

    /// 修复的回归测试：拖拽后 store 的 Vec 顺序必须**物理**改变 —— 仅
    /// 改 `t.order` 字段但 Vec 仍是插入顺序时，前端 `.filter()` 渲染
    /// 看不出区别。
    #[test]
    fn reorder_physically_reorders_vec() {
        let mut s = fresh_store(vec![
            mk("a", TodoStatus::Pending, 0),
            mk("b", TodoStatus::Pending, 1),
            mk("c", TodoStatus::Pending, 2),
        ]);
        s.reorder(&["c".into(), "a".into(), "b".into()]);
        assert_eq!(ids(&s), vec!["c", "a", "b"]);
        // section-local order 也在 0..N-1 重写
        assert_eq!(s.data.todos[0].order, 0);
        assert_eq!(s.data.todos[1].order, 1);
        assert_eq!(s.data.todos[2].order, 2);
    }

    /// 跨 status 拖拽：done 段重排不影响 pending 段的 Vec 位置。
    #[test]
    fn reorder_preserves_other_status_positions() {
        let mut s = fresh_store(vec![
            mk("a", TodoStatus::Pending, 0),
            mk("b", TodoStatus::Pending, 1),
            mk("c", TodoStatus::Done, 2),
            mk("d", TodoStatus::Done, 3),
        ]);
        // done 段从 [c, d] 重排为 [d, c]
        s.reorder(&["d".into(), "c".into()]);
        assert_eq!(ids(&s), vec!["a", "b", "d", "c"]);
        // pending 段 order 保持原值（没被拖到）
        assert_eq!(s.data.todos[0].order, 0);
        assert_eq!(s.data.todos[1].order, 1);
        // done 段被刷成 section-local 0, 1
        assert_eq!(s.data.todos[2].order, 0);
        assert_eq!(s.data.todos[3].order, 1);
    }

    /// 拖中间：拖的新顺序里既有 pending 也有不是本段的（API 防御）。
    /// 现在 pending 段只有 a,b,c —— 模拟 input 传了 done id 的坏情况，
    /// 这种 ids 找不到 → store 不动。
    #[test]
    fn reorder_no_op_when_ids_missing() {
        let mut s = fresh_store(vec![
            mk("a", TodoStatus::Pending, 0),
            mk("b", TodoStatus::Pending, 1),
        ]);
        s.reorder(&["z".into(), "x".into()]);  // 全是找不到的 id
        assert_eq!(ids(&s), vec!["a", "b"]);
        assert_eq!(s.data.todos[0].order, 0);
        assert_eq!(s.data.todos[1].order, 1);
    }

    /// 防御：`ids` 在 self.data.todos 里只找得到一部分 —— 整批拒绝，
    /// 不让 store 进入不一致。
    #[test]
    fn reorder_partial_match_aborts() {
        let mut s = fresh_store(vec![
            mk("a", TodoStatus::Pending, 0),
            mk("b", TodoStatus::Pending, 1),
        ]);
        s.reorder(&["a".into(), "ghost".into()]);
        // ghost 找不到 → 整批不动
        assert_eq!(ids(&s), vec!["a", "b"]);
    }

    /// 空 ids → no-op，不 crash。
    #[test]
    fn reorder_empty_is_noop() {
        let mut s = fresh_store(vec![mk("a", TodoStatus::Pending, 0)]);
        s.reorder(&[]);
        assert_eq!(ids(&s), vec!["a"]);
    }

    /// 拖"看似相同"：ids 顺序与现状相同 → 不应刷 updated_at（避免噪点）。
    /// 实现：用 mutate_count 代 updated_at 不好测，改用比较 updated_at 是否动过。
    #[test]
    fn reorder_no_updated_at_bump_when_position_unchanged() {
        let mut s = fresh_store(vec![
            mk("a", TodoStatus::Pending, 0),
            mk("b", TodoStatus::Pending, 1),
        ]);
        // 设 known updated_at：选一个远未来值，避免跟 now() 时间戳巧合
        let pinned = 1_700_000_000_000i64;
        for t in s.data.todos.iter_mut() {
            t.updated_at = pinned;
        }
        s.reorder(&["a".into(), "b".into()]);  // 同顺序
        // ids 没动 → updated_at 也不动
        for t in &s.data.todos {
            assert_eq!(t.updated_at, pinned, "no-op reorder bumped updated_at");
        }
    }

    /// 跨 status 边界（pending 段在 done 段之后）也能正确锚定。
    #[test]
    fn reorder_when_section_at_array_end() {
        // done 段在 Vec 末尾，拖它时 base_idx 指到尾段第一个 done
        let mut s = fresh_store(vec![
            mk("a", TodoStatus::Pending, 0),
            mk("b", TodoStatus::Done, 1),
            mk("c", TodoStatus::Done, 2),
        ]);
        s.reorder(&["c".into(), "b".into()]);
        assert_eq!(ids(&s), vec!["a", "c", "b"]);
        assert_eq!(s.data.todos[1].order, 0);
        assert_eq!(s.data.todos[2].order, 1);
    }

    /// 回归测试：用真实 todos.json（用户在 macOS 上的当前数据）模拟
    /// 一次 pending 段拖拽，确认 reorder 后 `data.todos` 的 Vec 顺序
    /// 真的变了 —— 之前 order 字段被写但 Vec 没动，前端 `.filter()`
    /// 取的还是旧顺序，所以拖了"无效果"。
    #[test]
    fn reorder_real_data_pending_changes_array_order() {
        let mut s = fresh_store(vec![
            mk("26", TodoStatus::Pending, 0),       // 26年百度智能云考试能力提升
            mk("123", TodoStatus::Pending, 1),      // 123123...
            mk("5", TodoStatus::Pending, 3),
            mk("6", TodoStatus::Pending, 4),
            mk("9", TodoStatus::Done, 2),
            mk("10", TodoStatus::Done, 3),
            mk("2", TodoStatus::Done, 5),
            mk("3", TodoStatus::Done, 6),
            mk("4", TodoStatus::Done, 7),
            mk("1", TodoStatus::Pending, 5),
        ]);
        // 模拟把 pending 段中"26"(index 0) 拖到"5"之后 —— section 内新顺序
        s.reorder(&[
            "123".into(),
            "5".into(),
            "26".into(),
            "6".into(),
            "1".into(),
        ]);
        // 验证 Vec 顺序变化（done 段位置不动）
        assert_eq!(ids(&s), vec![
            "123", "5", "26", "6", "1",
            "9", "10", "2", "3", "4",
        ]);
        // pending 段的 Vec 切片顺序 = 新顺序
        let pending: Vec<&str> = s.data.todos.iter()
            .filter(|t| t.status == TodoStatus::Pending)
            .map(|t| t.id.as_str())
            .collect();
        assert_eq!(pending, vec!["123", "5", "26", "6", "1"]);
        // done 段的 Vec 切片顺序 = 原序（未动）
        let done: Vec<&str> = s.data.todos.iter()
            .filter(|t| t.status == TodoStatus::Done)
            .map(|t| t.id.as_str())
            .collect();
        assert_eq!(done, vec!["9", "10", "2", "3", "4"]);
    }
}