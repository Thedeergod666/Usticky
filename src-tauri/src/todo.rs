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
use std::sync::Mutex;

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
pub struct Store {
    data: StoreData,
    data_path: Mutex<Option<PathBuf>>,
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

    /// 拖拽后批量更新 order。
    /// `ids` 是新顺序（按 status 内顺序传入，由 IPC caller 保证）。
    ///
    /// 只在 `t.order != i as i32` 时才刷新 updated_at —— 拖一下整 section
    /// 都不改 order 时（例如前端重排但 ids 没变），不应产生"更新时间"噪声。
    pub fn reorder(&mut self, ids: &[String]) {
        for (i, id) in ids.iter().enumerate() {
            if let Some(t) = self.data.todos.iter_mut().find(|t| &t.id == id) {
                let new_order = i as i32;
                if t.order != new_order {
                    t.order = new_order;
                    t.updated_at = chrono::Utc::now().timestamp_millis();
                }
            }
        }
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
    pub fn persist(&self, _app: &AppHandle) -> Result<()> {
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