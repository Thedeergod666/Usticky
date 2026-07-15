// Usticky 浮窗 —— index.html
//
// 渲染策略：增量 DOM diff（按 data-todo-id），避免 innerHTML 全量替换闪烁。
// 设计要点（详见 AGENTS.md 第 3 节）：
//   1. 绝不用 innerHTML 全量替换 → 走 buildTodoSkeleton + updateTodo diff
//   2. #app.scrollHeight 自适应（不用 documentElement.scrollHeight，否则反馈环涨高）
//   3. contentFingerprint 去重：数据刷新不动尺寸，只结构变化才 fit
//   4. 拖拽 todo 行 ≠ 拖窗：mousedown target 检查 .todo-card 才让 SortableJS 处理
//   5. 全局快捷键 CmdOrCtrl+Shift+Space → 聚焦 input
//
// 不复用 Musage 的：
//   - lastGoodSnap + TRANSIENT_ERROR_KINDS（todo 没有瞬态错误）
//   - 多 provider 调度（todo 就一个 list）
//   - PinBottom 私有 API（v0.1 不做）
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import Sortable from "sortablejs";
import { t, initLocale, onLocaleChange, setLocale, getLocale } from "./i18n";
import "./styles.css";

// ── 数据模型 ──
type TodoStatus = "pending" | "done";
type TodoPriority = "P0" | "P1" | "P2" | "P3";

interface Todo {
  id: string;          // UUID v4 (stable key for diff / drag)
  title: string;
  status: TodoStatus;
  priority: TodoPriority;
  created_at: number;  // epoch ms
  updated_at: number;
  due_at: number | null;
  tags: string[];
  order: number;       // 同 status 内独立排序
}

interface TodoSnapshot {
  todos: Todo[];
  fetched_at: number | null;
}

// ── 全局状态 ──
const app = document.getElementById("app")!;
let lastRenderedSnap: TodoSnapshot | null = null;
let lastFitFingerprint: string | null = null;
let sortableInstances: Sortable[] = [];  // 保存实例，cleanup 用
let isDragging = false;  // 自己跟踪 SortableJS 拖拽状态（比 sortableInstances[i].dragEl 可靠，
                          // SortableJS 偶尔会在某些边界场景里遗留 dragEl 不清，导致后续
                          // render 永远被 blocking）。进入 onStart 时置 true，onEnd 置 false。

/// SortableJS filter 用的"不可拖拽"子元素选择器 —— 命中即跳过拖拽判定，
/// 让原生 click 事件正常派发到这些按钮上。这是修复"完成/取消完成/删除
/// 被拖拽吞掉"的关键配置（详见 sortableOpts 注释）。
///
/// 未来新增"不应被 SortableJS 当作拖拽起点"的卡内控件（如优先级 chip、
/// 标签按钮），只需把选择器追加到这条常量里。
const NON_DRAGGABLE_SELECTORS = ".todo-check, .todo-delete, .todo-edit-input";

// 浮窗层级模式（pin_top / pin_bottom / normal）
// 启动时从后端 get_pin_mode 拉，跟 usticky://pin-mode-changed 事件同步
type PinMode = "pin_top" | "pin_bottom" | "normal";
let currentPinMode: PinMode = "pin_bottom";  // 默认跟后端 PinMode::default() 对齐

// 当前 quick-add 快捷键（accelerator 字符串，如 "Cmd+Shift+Space"）
// 启动时从后端 get_quick_add_shortcut 拉，跟 usticky://shortcut-changed 事件同步。
// 用于 input hint 显示（替换原来写死的 i18n 字符串 "⌘⇧Space"）。
let currentShortcut: string = "Cmd+Shift+Space";

/// 把 accelerator 字符串转展示形式（macOS: `⌘⇧Space`）。
/// 跟 settings.ts 同款逻辑，前端两个 webview 各实现一份（避免引入共享 module）。
function formatShortcutForDisplay(s: string): string {
  const isMac = /mac/i.test(navigator.platform);
  if (!isMac) return s;
  const parts = s.split("+").map((p) => p.trim());
  let out = "";
  for (const p of parts) {
    switch (p.toLowerCase()) {
      case "cmd": case "command": case "super": case "meta":
      case "cmdorctrl": case "cmdorcontrol":
      case "commandorctrl": case "commandorcontrol":
        out += "⌘"; break;
      case "ctrl": case "control":
        out += "⌃"; break;
      case "shift":
        out += "⇧"; break;
      case "alt": case "option": case "opt":
        out += "⌥"; break;
      default:
        out += p;
    }
  }
  return out;
}

/// 最窄档（tier 2）专用的图标版快捷键：只保留 modifier，去掉 key letter。
/// `Cmd+Shift+Space` → `⌘⇧`（省 ~14px 字符宽度，让 240px 浮窗能 fit）。
/// 非 Mac 平台：直接去掉最后一截 key（保留修饰键文本），跟 macOS icon 版语义对齐。
function formatShortcutIcon(s: string): string {
  const isMac = /mac/i.test(navigator.platform);
  if (!isMac) {
    const parts = s.split("+").map((p) => p.trim());
    // 保留所有 modifier，丢掉最后一截 key 字母
    return parts.slice(0, -1).join("+");
  }
  // Mac：复用 full 版的格式器，但跳过最后一个 part（key 字母）
  const parts = s.split("+").map((p) => p.trim());
  let out = "";
  for (let i = 0; i < parts.length - 1; i++) {
    const p = parts[i];
    switch (p.toLowerCase()) {
      case "cmd": case "command": case "super": case "meta":
      case "cmdorctrl": case "cmdorcontrol":
      case "commandorctrl": case "commandorcontrol":
        out += "⌘"; break;
      case "ctrl": case "control":
        out += "⌃"; break;
      case "shift":
        out += "⇧"; break;
      case "alt": case "option": case "opt":
        out += "⌥"; break;
      default:
        out += p;  // 非 modifier 也不该出现，防御性保留
    }
  }
  return out;
}

/// 浮窗宽度的"窄窗档位"（CSS 据此调 hint 字号 / 文字 / 容器 padding）。
type NarrowTier = 0 | 1;

/// 浮窗宽度档位阈值（px）。
///
///  - tier 0（正常，≥ NARROW_THRESHOLD）：hint = 10px + 背景框 + '⌘⇧Space'
///  - tier 1（图标，w < NARROW_THRESHOLD）：
///          hint = 9px + 无背景 + '⌘⇧'（去 'Space' 文字，再省 ~20px）
///
/// 两档而不是三档的原因：用户实测 240-279px 区间 compact 版
/// '⌘⇧Space' 仍被推出浮窗 —— 中文 13px 字号下 input 占位符
/// '要做的事？回车保存' 实际渲染 ~135px（比英文 'What needs doing?
/// Press Enter' 宽 ~25px），加上 hint '⌘⇧Space' (~46px) + gap + padding
/// 在 260px 就开始裁字。tier 1 阈值取 minWidth 240 + 40px = 280，
/// 280 以下 hint 直接 '⌘⇧' 图标版，足够 fit。
const NARROW_THRESHOLD = 280;

/// 算当前宽度的档位。
function computeNarrowTier(): NarrowTier {
  const w = window.innerWidth;
  if (w < NARROW_THRESHOLD) return 1;
  return 0;
}

/// 把 input hint 刷成 currentShortcut 的展示形式。
/// tier=1 时用图标版（`⌘⇧` 去 'Space'）省字符宽度。
/// tier=0 用完整版（`⌘⇧Space`）保信息密度。
function updateInputHint(tier: NarrowTier = 0) {
  const hint = app.querySelector<HTMLElement>(".todo-input-hint");
  if (hint) {
    hint.textContent = tier === 1
      ? formatShortcutIcon(currentShortcut)
      : formatShortcutForDisplay(currentShortcut);
  }
}

/// 根据 `window.innerWidth` 切档：设 `body[data-narrow]` 触发 CSS，
/// 同时调 `updateInputHint(tier)` 切文字（tier 1 时 hint 变 `⌘⇧`）。
///
/// 两档策略：
///   - 不用 display:none 隐藏 hint（用户期望"图标不被挤压"）
///   - tier 1 直接用 '⌘⇧' 图标版，不用 '⌘⇧Space' 完整版
///     （240-279 区间中文系统实测 fit 不了完整版）
///   - input 字号不变（用户已习惯 13px + 占位符宽度节奏）
function syncNarrowMode() {
  const tier = computeNarrowTier();
  document.body.dataset.narrow = tier === 0 ? "" : "1";
  updateInputHint(tier);
}

// ── mini flash（复用 Musage 模式） ──
let miniFlashTimer: ReturnType<typeof setTimeout> | null = null;
function showMiniFlash(msg: string): void {
  let el = app.querySelector<HTMLElement>(".mini-flash");
  if (!el) {
    el = document.createElement("div");
    el.className = "mini-flash";
    app.appendChild(el);
  }
  el.textContent = msg;
  el.classList.add("visible");
  if (miniFlashTimer) clearTimeout(miniFlashTimer);
  miniFlashTimer = setTimeout(() => el?.classList.remove("visible"), 3000);
}

// ── 工具 ──
function escapeHtml(s: string): string {
  return s.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]!));
}

function cssEscape(s: string): string {
  if (typeof (CSS as any).escape === "function") return (CSS as any).escape(s);
  return s.replace(/([!"#$%&'()*+,./:;<=>?@[\]^`{|}~])/g, "\\$1");
}

/// 把 Rust 上报的屏幕 logical 坐标转成 viewport 相对坐标，再喂给
/// `document.elementFromPoint`。
///
/// 跨平台 Y 轴差异：
///   - **macOS**：`NSEvent.mouseLocation()` 是 bottom-left origin
///     of **main display**（global screen coords，logical points），
///     跟 CSS `elementFromPoint` 期待的 top-left 反着。要先
///     `screenH - y` 翻一次到 top-origin。
///   - **Win**：`GetCursorPos` 已是 top-left origin 且 Rust 已转 logical，
///     直接用。
///
/// `winY` 来自 Tauri `innerPosition()`（tao 0.35.3
/// `bottom_left_to_top_left`），该函数用 `pixels_high()`（物理像素）
/// 减 `origin.y + height`（logical points）→ mixed-unit。对 Retina
/// scale=2，返回值比正确的 frame top Y (in main top-origin logical)
/// 多了 `fullScreenH`。所以这里不传修正后的 winY，而是直接在公式里
/// 用 `fullScreenH*2 - screenY - winY` 把 two-fold 误差消掉：
/// 正确的 `fullScreenH - screenY - (winY - fullScreenH)`
/// = `fullScreenH*2 - screenY - winY`。
/// 多屏场景同样适用（无论在主屏/副屏，tao 的误差都 = 主屏 fullScreenH）。
function screenToViewportCoords(
  screenX: number,
  screenY: number,
  winX: number,
  winY: number,
): { x: number; y: number } {
  const isMac = /mac/i.test(navigator.platform);
  if (isMac) {
    // screenH = 主屏 logical height（full screen，含 menu bar）。
    // mouseLocation 是 main bottom-origin → `screenH - screenY` 翻到
    // main top-origin → 再减 winY（已经是修过的 frame top Y in main top-origin
    // logical）→ viewport-relative Y。
    const fullScreenH = window.screen?.height ?? window.innerHeight;
    // tao 0.35.3 `bottom_left_to_top_left` 用 `pixels_high()`（物理像素）
    // 减 `origin.y + height`（logical points）→ mixed-unit → 对 Retina
    // scale=2 多 fullScreenH。`cachedWinY` 比正确值多了 fullScreenH，
    // 所以这里要补一个 fullScreenH：2*H - sy - cy。
    return { x: screenX - winX, y: fullScreenH * 2 - screenY - winY };
  }
  return { x: screenX - winX, y: screenY - winY };
}

/// 内容指纹 —— 只看结构维度（todo 数 / 状态 / due），不看 title 文本。
/// title 改了 → 不触发 autoResize（保持用户手动尺寸）
function contentFingerprint(snap: TodoSnapshot): string {
  return snap.todos
    .map((t) => `${t.status}|${t.due_at ? "due" : "no-due"}|${t.priority}`)
    .join(";");
}

/// due_at 转人类可读标签（按当前时间动态计算）
function dueLabel(dueAt: number): { text: string; cls: string } {
  const now = Date.now();
  const ms = dueAt - now;
  const dueDate = new Date(dueAt);
  const todayStart = new Date(); todayStart.setHours(0, 0, 0, 0);
  const tomorrowStart = new Date(todayStart.getTime() + 86400000);
  const daysFromToday = Math.floor((dueDate.getTime() - todayStart.getTime()) / 86400000);

  if (ms <= 0) return { text: t("app.due.overdue"), cls: "overdue" };
  if (dueDate < tomorrowStart && dueDate >= todayStart) {
    return { text: t("app.due.today"), cls: "today" };
  }
  if (daysFromToday === 1) return { text: t("app.due.tomorrow"), cls: "tomorrow" };
  if (daysFromToday >= 2 && daysFromToday <= 7) {
    return { text: t("app.due.days", { days: daysFromToday }), cls: "days" };
  }
  // 远期：按"天"展示（i18n 字典 app.due.future / app.due.hours_minutes 已有 key）
  if (daysFromToday > 7) {
    return { text: t("app.due.future", { days: daysFromToday }), cls: "days" };
  }
  // 精度更高（今天内 / 几小时内）
  const minutes = Math.floor(ms / 60000);
  if (minutes < 60) return { text: t("app.due.minutes", { minutes }), cls: "days" };
  const hours = Math.floor(minutes / 60);
  const mins = minutes % 60;
  return { text: t("app.due.hours_minutes", { hours, minutes: mins }), cls: "days" };
}

// ── 渲染 ──

function render(snap: TodoSnapshot) {
  lastRenderedSnap = snap;

  // 首启空态
  if (snap.todos.length === 0) {
    renderEmptyState();
    return;
  }

  // 拖拽期间跳过一次重建，待 onEnd 后由 emit("todos-changed") 走下
  // 一次 render 统一刷新。自己跟踪 `isDragging` 而不是读 SortableJS
  // 的 `dragEl` —— forceFallback 下 dragEl 时序跟 DOM 事件不同步，
  // 上一版用 `sortableInstances[i].dragEl` 偶尔会被 SortableJS 留在
  // 旧状态卡住，导致后续 render 全部被挡。
  if (isDragging) return;

  // 清掉旧内容（保留 input / foot）
  cleanupSortables();
  const oldList = app.querySelector<HTMLElement>(".todo-list");
  if (oldList) oldList.remove();
  // 切到非空时把空态引导页摘掉
  app.querySelector<HTMLElement>(".empty-state")?.remove();

  const list = document.createElement("div");
  list.className = "todo-list";
  list.style.display = "flex";
  list.style.flexDirection = "column";
  list.style.gap = "6px";

  const pending = snap.todos.filter((t) => t.status === "pending");
  const done = snap.todos.filter((t) => t.status === "done");

  if (pending.length > 0) {
    const header = document.createElement("div");
    header.className = "section-header";
    header.textContent = `${t("app.section.pending")} · ${pending.length}`;
    list.appendChild(header);
    const ul = document.createElement("div");
    ul.className = "todo-section todo-section-pending";
    ul.style.display = "flex";
    ul.style.flexDirection = "column";
    ul.style.gap = "6px";
    for (const todo of pending) ul.appendChild(buildTodoRow(todo));
    list.appendChild(ul);
  } else {
    const empty = document.createElement("div");
    empty.className = "section-header";
    empty.textContent = t("app.empty.pending");
    list.appendChild(empty);
  }

  if (done.length > 0) {
    const header = document.createElement("div");
    // 多加 .section-header-done —— idle（未 hover）下 CSS 把这条 header
    // 完全透明（包括 "DONE · N" 文字），让 done 区块在浮窗静止时彻底
    // "折叠"到背景里（标题文字也已划掉且弱化，header 自身再隐藏
    // = 完成区几乎只剩几道勾圈痕迹）。hover 时回归可见。
    header.className = "section-header section-header-done";
    header.textContent = `${t("app.section.done")} · ${done.length}`;
    list.appendChild(header);
    const ul = document.createElement("div");
    ul.className = "todo-section todo-section-done";
    ul.style.display = "flex";
    ul.style.flexDirection = "column";
    ul.style.gap = "6px";
    for (const todo of done) ul.appendChild(buildTodoRow(todo));
    list.appendChild(ul);
  }

  app.appendChild(list);

  // 挂 SortableJS。
  //
  // 关键选项：
  //   - `forceFallback: true`：用 SortableJS 自己的 mousedown/move/up
  //     实现，不用 HTML5 drag-and-drop。原因：Tauri WKWebView + macOS
  //     上 HTML5 dragstart 在拖出卡片边缘时被 `-webkit-user-drag` /
  //     `cursor: grab` 等样式吃掉，导致 `onEnd` 不触发 / 延迟触发；
  //     fallback 路径走纯鼠标事件 + ghost 元素，跨平台一致。
  //   - `ghostClass: "dragging"`：拖拽时占位元素加 `.dragging` class，
  //     styles.css 把它设成半透明 + 微缩，提醒用户"这张是 ghost"。
  //   - `animation: 150`：松手后归位动画（CSS transform）。
  //   - `onEnd: handleDragEnd`：发 reorder_todos IPC。
  //   - `filter: NON_DRAGGABLE_SELECTORS`：在 mousedown 阶段如果 target 命中这些
  //     选择器，SortableJS 立即终止当前拖拽候选，**不**进入 `.sortable-chosen`
  //     态。这是修复"完成/取消完成/删除被吞"的关键：原版 SortableJS 在
  //     mousedown 阶段会 preventDefault 后续 click 事件，且把整个 .todo-card
  //     切到 chosen 态（即便用户其实是想点 check/delete 按钮）。filter 让
  //     SortableJS 在按钮 mousedown 时直接放手，click 正常派发到按钮。
  //   - `preventOnFilter: true`：命中 filter 时让原生事件继续走（click 仍能
  //     派发到 checkbox / delete），而不是被 SortableJS 拦死。
  const sortableOpts: Sortable.Options = {
    animation: 150,
    ghostClass: "dragging",
    forceFallback: true,
    fallbackOnBody: true,
    filter: NON_DRAGGABLE_SELECTORS,
    preventOnFilter: true,
    onStart: () => { isDragging = true; },
    onEnd: (evt) => {
      isDragging = false;
      void handleDragEnd(evt);
    },
  };
  const pendingSection = list.querySelector<HTMLElement>(".todo-section-pending");
  if (pendingSection) {
    sortableInstances.push(new Sortable(pendingSection, sortableOpts));
  }
  const doneSection = list.querySelector<HTMLElement>(".todo-section-done");
  if (doneSection) {
    sortableInstances.push(new Sortable(doneSection, sortableOpts));
  }

  // 输入中禁止 autoResize —— scrollHeight 跳变会打断输入（AGENTS.md #18）
  // 但"输入中"的判定是 input 有内容（正在打字），不是聚焦本身 ——
  // Enter 添加后 input 仍聚焦但已清空，此时 resize 是安全的，否则
  // "添加新 todo"永远等不到 resize（用户每次按 Enter 都被这里挡掉）。
  const inputEl = app.querySelector<HTMLInputElement>(".todo-input input");
  const typing = inputEl?.matches(":focus") && (inputEl.value.length > 0);
  if (!typing) {
    void autoResizeWindow(snap);
  }
}

function buildTodoRow(todo: Todo): HTMLElement {
  const row = document.createElement("div");
  row.className = `todo-card${todo.status === "done" ? " done" : ""}`;
  row.dataset.todoId = todo.id;
  row.dataset.status = todo.status;

  const check = document.createElement("div");
  check.className = "todo-check";
  check.title = todo.status === "done" ? t("app.action.undo") : t("app.action.complete");
  check.addEventListener("click", (e) => {
    e.stopPropagation();
    toggleDone(todo);
  });
  row.appendChild(check);

  const title = document.createElement("div");
  title.className = "todo-title";
  title.textContent = todo.title;
  title.title = todo.title;  // hover tooltip for truncated
  // 点击 title 区域进入编辑态（不要影响 .todo-check / .todo-delete 上的
  // 完成/删除按钮 —— 它们各自 stopPropagation 已在上面挂好）。
  // 单击即触发；不用 dblclick，避免"先点没反应"的体感。
  title.addEventListener("click", (e) => {
    e.stopPropagation();
    enterEditMode(row, todo);
  });
  row.appendChild(title);

  if (todo.due_at) {
    const due = document.createElement("div");
    const { text, cls } = dueLabel(todo.due_at);
    due.className = `todo-due ${cls}`;
    due.textContent = text;
    row.appendChild(due);
  }

  const del = document.createElement("button");
  del.className = "todo-delete";
  del.textContent = "×";
  del.title = t("app.action.delete");
  del.addEventListener("click", (e) => {
    e.stopPropagation();
    deleteTodo(todo);
  });
  row.appendChild(del);

  // hover-expand：卡片 hover 时展开截断的 title，离开时收回。
  // 复用 scheduleHoverResize 的 rAF coalescing + delta 检测，
  // 同帧跨卡切换只触发一次 resize IPC，视觉无抖动。
  row.addEventListener("mouseenter", () => {
    scheduleHoverResize(row, true);
  });
  row.addEventListener("mouseleave", () => {
    scheduleHoverResize(row, false);
  });

  return row;
}

function renderEmptyState() {
  cleanupSortables();
  // 关键：只清掉 .todo-list 和旧 .empty-state，**保留** .todo-input 和 .foot
  // （旧实现 app.innerHTML = ... 会把 ensureInputBar 建的 input bar 整个冲掉，
  //  导致首启空态时 input 不可见、CTA 点了也找不到 input → "addTask 没反应"）
  app.querySelector<HTMLElement>(".todo-list")?.remove();
  app.querySelector<HTMLElement>(".empty-state")?.remove();

  const empty = document.createElement("div");
  empty.className = "empty-state";

  const title = document.createElement("div");
  title.className = "empty-state-title";
  title.textContent = t("app.empty.title");

  const subtitle = document.createElement("div");
  subtitle.className = "empty-state-subtitle";
  subtitle.textContent = t("app.empty.subtitle");

  const cta = document.createElement("button");
  cta.className = "empty-state-cta focus-input";
  cta.textContent = t("app.empty.cta");

  empty.appendChild(title);
  empty.appendChild(subtitle);
  empty.appendChild(cta);

  app.appendChild(empty);

  if (!app.querySelector<HTMLInputElement>(".todo-input input")?.matches(":focus")) {
    void autoResizeWindowToContent();
  }
}

// ── 自适应高度 ──

/// hover-expand：截断的 title 在卡片 hover 时换行展开（独立路径，绕开
/// autoResizeWindow 的 contentFingerprint 去重 —— 文本变化不该触发结构
/// resize，但 hover 是用户主动意图，每次都要按需 fit）。
const TITLE_EXPAND_CLASS = "title-expanded";
// delta < 4px 视为不变化，跳过 resize（避免 1px 抖动触发 IPC）
const HOVER_RESIZE_DELTA_PX = 4;
// rAF coalescing：同一帧内多次 mouseenter/mouseleave 只触发一次 resize
let hoverResizePending = false;

/// 把卡片切到 expanded/collapsed 态，rAF 后测 delta 决定是否 resize。
///
/// 不在 input 打字时调用（typing 检测跟 render 一致 line 266-270），
/// 不在 vanish 动画中调用，幂等切换（已是目标态直接返回）。
///
/// 未截断的 title 也走这条路径 —— toggle class 后视觉不变，
/// scrollHeight delta = 0，delta 检测自然 skip resize，零副作用。
function scheduleHoverResize(card: HTMLElement, expanding: boolean) {
  const appEl = document.getElementById("app");
  if (!appEl) return;

  // 防御：节点已在 vanish 动画中（被删 / 即将被删）
  if (card.classList.contains("vanishing")) return;
  if (!card.isConnected) return;

  // 输入中禁止 resize —— 跟 line 266-270 的 typing 检测保持一致
  const inputEl = app.querySelector<HTMLInputElement>(".todo-input input");
  if (inputEl?.matches(":focus") && inputEl.value.length > 0) return;

  // 已经是目标态 → 跳过（鼠标快速进出同一卡 / 重复 enter）
  const wasExpanded = card.classList.contains(TITLE_EXPAND_CLASS);
  if (expanding === wasExpanded) return;

  const beforeH = appEl.scrollHeight;
  card.classList.toggle(TITLE_EXPAND_CLASS, expanding);

  // rAF coalescing：同一帧内多次调用只测一次 / resize 一次
  if (hoverResizePending) return;
  hoverResizePending = true;
  requestAnimationFrame(() => {
    hoverResizePending = false;
    const afterH = appEl.scrollHeight;
    const delta = afterH - beforeH;
    if (Math.abs(delta) < HOVER_RESIZE_DELTA_PX) return;
    void resizeWindowToContent(appEl);
  });
}

async function autoResizeWindow(snap: TodoSnapshot) {
  await new Promise<void>((r) => requestAnimationFrame(() => r()));
  const appEl = document.getElementById("app");
  if (!appEl) return;
  const fp = contentFingerprint(snap);
  if (fp === lastFitFingerprint) return;
  lastFitFingerprint = fp;
  await resizeWindowToContent(appEl);
}

async function autoResizeWindowToContent() {
  await new Promise<void>((r) => requestAnimationFrame(() => r()));
  const appEl = document.getElementById("app");
  if (!appEl) return;
  await resizeWindowToContent(appEl);
}

async function resizeWindowToContent(appEl: HTMLElement) {
  const contentH = appEl.scrollHeight;
  const screenH = window.screen?.availHeight ?? 2400;
  const maxH = Math.max(200, screenH - 80);
  const currentH = window.innerHeight;
  const desired = Math.min(contentH, maxH);
  if (Math.abs(currentH - desired) <= 1) return;
  try {
    await invoke("resize_floating_window", { height: Math.round(desired) });
  } catch (e) {
    console.debug("[usticky] auto-resize 失败", e);
  }
}

// ── 操作 ──
async function addTodo(title: string) {
  const trimmed = title.trim();
  if (!trimmed) {
    showMiniFlash(t("app.error.empty_title"));
    return;
  }
  if (trimmed.length > 280) {
    showMiniFlash(t("app.error.too_long", { max: 280 }));
    return;
  }
  try {
    await invoke("add_todo", { title: trimmed });
  } catch (e) {
    console.error("[usticky] add_todo failed", e);
    showMiniFlash(t("app.error.save_failed"));
  }
}

async function toggleDone(todo: Todo) {
  // 乐观更新 DOM：先加 .vanishing 动画（变成一条线 → 继续缩窄到 0），
  // 动画结束后再调 IPC。500ms 匹配 CSS @keyframes vanish-to-line 总时长。
  const row = app.querySelector<HTMLElement>(`.todo-card[data-todo-id="${cssEscape(todo.id)}"]`);
  if (todo.status === "pending") {
    // 标完成：pending 行 vanishing → done section 出现新行
    if (row) {
      row.classList.add("vanishing");
      setTimeout(async () => {
        try {
          await invoke("update_todo", { id: todo.id, status: "done" });
        } catch (e) {
          console.error("[usticky] update_todo failed", e);
          row.classList.remove("vanishing");
        }
      }, 500);
    } else {
      await invoke("update_todo", { id: todo.id, status: "done" });
    }
  } else {
    // 撤销完成：done 行 vanishing（变成一条线 → 继续缩窄）→ pending section 出现新行
    // 即"done 移到 pending 行，原有 done 行变成一条线并继续缩窄高度"
    if (row) {
      row.classList.add("vanishing");
      setTimeout(async () => {
        try {
          await invoke("update_todo", { id: todo.id, status: "pending" });
          showMiniFlash(t("app.undo.flash", { title: todo.title }));
        } catch (e) {
          console.error("[usticky] undo failed", e);
          row.classList.remove("vanishing");
        }
      }, 500);
    } else {
      try {
        await invoke("update_todo", { id: todo.id, status: "pending" });
        showMiniFlash(t("app.undo.flash", { title: todo.title }));
      } catch (e) {
        console.error("[usticky] undo failed", e);
      }
    }
  }
}

async function deleteTodo(todo: Todo) {
  const row = app.querySelector<HTMLElement>(`.todo-card[data-todo-id="${cssEscape(todo.id)}"]`);
  if (row) {
    row.classList.add("vanishing");
    setTimeout(async () => {
      try {
        await invoke("delete_todo", { id: todo.id });
        showMiniFlash(t("app.delete.flash", { title: todo.title }));
      } catch (e) {
        console.error("[usticky] delete_todo failed", e);
        row.classList.remove("vanishing");
      }
    }, 500);
  } else {
    await invoke("delete_todo", { id: todo.id });
  }
}

async function handleDragEnd(evt: Sortable.SortableEvent) {
  // SortableJS `evt.to` 在 onEnd 触发时已经是被移动后的 DOM，所以直接
  // querySelectorAll 拿到的是最终位置（前段原始 → 落入新位置 → onEnd），
  // 跟视觉顺序一致。
  const section = (evt.to as HTMLElement).closest<HTMLElement>(".todo-section")
    ?? (evt.to as HTMLElement);
  const ids: string[] = [];
  section.querySelectorAll<HTMLElement>(".todo-card").forEach((el) => {
    if (el.dataset.todoId) ids.push(el.dataset.todoId);
  });
  // 防御：不到 2 条的拖拽（理论上极少发生：用户拖了 = 至少 2 条在 section）
  // 不浪费一次 IPC；单条 reorder 也是 no-op 但无害，省一次往返。
  if (ids.length < 2) return;
  try {
    await invoke("reorder_todos", { ids });
    // 成功后不强推 render —— 后端 emit("usticky://todos-changed") 会
    // 走到 frontend listener 触发 render。重复 render 在某些路径下会
    // 跟 SortableJS 的内部 DOM 状态机打架（dragEl 判断会出现"刚
    // onEnd 完 dragEl 可能还没被清除"的窗口期）。
  } catch (e) {
    console.error("[usticky] reorder_todos failed", e);
  }
}

function cleanupSortables() {
  for (const s of sortableInstances) s.destroy();
  sortableInstances = [];
  // 防御：cleanup 时如果还有遗留的 dragging 状态（理论上 should clear
  // onEnd 已经处理，但跨 webview 某些边界 / 异常路径可能遗漏），重置，
  // 不让后续 render 永久被挡。
  isDragging = false;
}

// ── 编辑 todo ──
//
// 设计要点：
//   1. 触发：点击 .todo-title（非 checkbox / delete 按钮）。
//   2. UI：原 .todo-title 替换为 <input>，in-place 编辑（不重建整卡，
//      不打断 SortableJS 实例 / hover-expand 状态）。
//   3. SortableJS：编辑期间禁用所在 section 的 Sortable，
//      否则 mousedown 到 input 上 SortableJS 会启动一个无效的拖拽。
//   4. 提交：Enter / blur；空内容 / 未改动 → 视为取消。
//   5. 取消：Escape 键。
//   6. 错误处理：保存失败回滚 input value（保留用户已输入文字），不闪退。
//   7. 与 hover-expand 协作：编辑态期间关闭该卡的 title-expanded，避免
//      input 框被外层 .title-expanded 的 white-space: normal 干扰。
//
// 不需要的数据模型改动：后端 update_todo 早就有 title: Option<String>
// 参数，commands/mod.rs line 81-100 直接复用。
function enterEditMode(card: HTMLElement, todo: Todo) {
  // 已有 input 在编辑（防御性 —— 多次快速点击）
  if (card.querySelector("input.todo-edit-input")) return;

  const titleEl = card.querySelector<HTMLElement>(".todo-title");
  if (!titleEl) return;

  // 关闭 hover-expand 态（如果正展开中）—— input 编辑不应该被外层换行影响
  card.classList.remove("title-expanded");

  // 禁用所在 section 的 SortableJS —— 否则 mousedown 到 input 上
  // Sortable 会启动一个无效拖拽（input 没有 draggable 元素 + 拖动会被
  // input 的 focus 行为打断）。
  const section = card.closest<HTMLElement>(".todo-section");
  if (section) {
    const sortInstance = sortableInstances.find((s) => (s as any).el === section);
    if (sortInstance) sortInstance.option("disabled", true);
  }

  const input = document.createElement("input");
  input.type = "text";
  input.className = "todo-edit-input";
  input.maxLength = 280;
  input.value = todo.title;
  input.spellcheck = false;
  input.autocomplete = "off";
  // 显式 user-select：input 默认可输入文字，与外层 user-select: none 区分
  // 视觉上 input 边框 + focus ring 已经够标识编辑态。

  // 把 title 节点换成 input，保留原引用以便 cancel 时换回。
  titleEl.replaceWith(input);

  // focus + select：用户能直接覆盖或追加
  // 用 rAF 让 select 在 macOS Safari 行为一致（focus 后立刻 select 不被
  // Safari 当成"二次 focus"吞掉）。
  requestAnimationFrame(() => {
    input.focus();
    input.select();
  });

  // 提交（Enter 或 blur）—— 共享一个 handler
  let settled = false;
  const commit = async () => {
    if (settled) return;
    settled = true;
    const next = input.value.trim();
    // 空内容 / 未改动 → 视为取消，不发 IPC 也不闪退
    if (!next || next === todo.title) {
      cancelEdit(card, todo);
      return;
    }
    // 字符上限校验（跟 addTodo 一致）
    if (next.length > 280) {
      showMiniFlash(t("app.error.too_long", { max: 280 }));
      settled = false;  // 允许重新提交
      input.focus();
      return;
    }
    // 保留用户输入：commit 失败时不替换 input，继续让用户修改
    try {
      // 先 exit 再 invoke —— 后端 persist_and_emit 同步触发
      // usticky://todos-changed 走 render 整张重建。如果先 invoke 再 exit，
      // 重建会把 input 干掉，然后我们的 exit 跑在 detached card 上。
      exitEditMode(card, next);
      await invoke("update_todo", { id: todo.id, title: next });
      // 成功：后端 emit 会重建整张列表，next 已落盘
    } catch (e) {
      console.error("[usticky] update_todo (edit) failed", e);
      // 失败：重新进入编辑态恢复 input（exit 已经把 input 摘了）
      showMiniFlash(t("app.error.save_failed"));
      settled = false;
      // 找当前 card（render 还没跑的话还是原来的；跑了的话是新卡），
      // 用原 id 重新进入编辑 —— 但原 todo 对象还是同一份，title 没变
      reenterEditOnFailure(card, todo, next);
    }
  };

  const cancel = () => {
    if (settled) return;
    settled = true;
    cancelEdit(card, todo);
  };

  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter") {
      e.preventDefault();
      e.stopPropagation();
      void commit();
    } else if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      cancel();
    }
  });
  // blur 提交 —— 注意：commit 内部 settled 检查避免重复触发
  // （cancel 后元素被移走，blur 仍可能在异步队列里派发）
  input.addEventListener("blur", () => {
    void commit();
  });
}

/// 失败后恢复编辑态 —— 重新把 title 换成 input 并 focus。
/// card 可能是 detached（render 已重建），querySelector 找不到时跳过
/// （用户已经看不到这张卡了，强行恢复会让界面更乱）。
function reenterEditOnFailure(card: HTMLElement, todo: Todo, value: string) {
  if (!card.isConnected) return;
  // 找到当前还在 DOM 的同 id card
  const live = card.dataset.todoId
    ? document.querySelector<HTMLElement>(
        `.todo-card[data-todo-id="${cssEscape(card.dataset.todoId)}"]`,
      )
    : null;
  const target = live ?? card;
  enterEditMode(target, todo);
  const input = target.querySelector<HTMLInputElement>("input.todo-edit-input");
  if (input) {
    input.value = value;
    input.focus();
  }
}

/// 退出编辑态（恢复成显示态），不调 IPC。用于 commit 成功后本地先 revert
/// 然后等待后端 emit 的 todos-changed 走完整重建。
function exitEditMode(card: HTMLElement, newTitle: string) {
  const input = card.querySelector<HTMLInputElement>("input.todo-edit-input");
  if (!input) {
    restoreSortableForCard(card);
    return;
  }
  const titleEl = document.createElement("div");
  titleEl.className = "todo-title";
  titleEl.textContent = newTitle;
  titleEl.title = newTitle;
  // 重建 title 上的点击监听 —— 跟 buildTodoRow 行为一致
  // 重新 query 拿 todo（从 card.dataset.todoId 反查 lastRenderedSnap）
  const id = card.dataset.todoId;
  const todo = id ? lastRenderedSnap?.todos.find((t) => t.id === id) : null;
  if (todo) {
    titleEl.addEventListener("click", (e) => {
      e.stopPropagation();
      enterEditMode(card, todo);
    });
  }
  input.replaceWith(titleEl);
  restoreSortableForCard(card);
}

/// 取消编辑 —— 把 input 换回原 title 显示
function cancelEdit(card: HTMLElement, todo: Todo) {
  exitEditMode(card, todo.title);
}

/// 恢复 Sortable —— 所有 section 的 Sortable 重新启用。
///
/// 用 find 而非 section 局部匹配：编辑期间如果用户切了排序（不应该发生，
/// 但防御性），需要在退出编辑时找当前 section 的实例。
function restoreSortableForCard(card: HTMLElement) {
  const section = card.closest<HTMLElement>(".todo-section");
  if (!section) return;
  const inst = sortableInstances.find((s) => (s as any).el === section);
  if (inst) inst.option("disabled", false);
}

// ── 启动 ──
async function init() {
  // i18n 必须在任何 t() 之前
  await initLocale();
  document.title = t("app.title");

  onLocaleChange(() => {
    document.title = t("app.title");
    // input placeholder 也需要随 locale 刷（创建时写死的）
    // hint 走 currentShortcut（不再用 i18n 的 shortcut_hint），不随 locale 变
    const input = app.querySelector<HTMLInputElement>(".todo-input input");
    if (input) input.placeholder = t("app.input.placeholder");
    if (lastRenderedSnap) render(lastRenderedSnap);
  });

  let unlistenLocale: UnlistenFn | null = null;
  listen<string>("usticky://locale-changed", async (e) => {
    const newLocale = e.payload;
    if (newLocale === "en" || newLocale === "zh-CN") {
      if (newLocale !== getLocale()) await setLocale(newLocale);
    }
  })
    .then((fn) => (unlistenLocale = fn))
    .catch((e) => console.error("[usticky] listen locale-changed failed", e));

  // ── Hover 状态同步：驱动 body[data-hover] 让 iOS 26 玻璃效果生效 ──
  //
  // 双路径并存（先到先生效，幂等）：
  //   1. Rust `usticky://floating-hover`（macOS 必需 —— WKWebView 非 key window
  //      不分发 mouseMoved，CSS `:hover` 在浮窗未聚焦时失效，Rust 用
  //      NSEvent.mouseLocation 全局轮询绕过）
  //   2. JS mouseenter/mouseleave（Win/Linux 主路径；macOS 聚焦态下兜底）
  //
  // 沿用 Musage fix 的两层保险（v0.2.x 闪烁修复）：
  //   (a) 页面隐藏时把 body mouseenter 视为 spurious 忽略（visibilitychange
  //       主动清 hover）。
  //   (b) hover 显形 40ms debounce —— enter→leave 抖动被吞；正常 hover 仅
  //       延后 40ms 不可察觉。leave 方向不 debounce（撤销要快）。
  //   (c) Rust emit 同值去重 —— hover emitter 内部已有 dwell-time
  //       hysteresis（enter 3 ticks / exit 2 ticks），这里再做一层保险
  //       避免 CSS spring 动画进行中反复重置起始点。
  //
  // **2026-07-06 fix**：去掉 `!focused` 守卫。原来的守卫目的是挡 deactivate→
  // reactivate 切换瞬间的 spurious mouseenter，但**副作用是 un-focused 窗口
  // 的合法 hover 全部被吞** —— 用户 hover 浮窗时鼠标进入事件被 Rust / JS
  // 双路径同时挡掉，必须点一下让窗口获焦后第二次 hover 才生效，体验上就是
  // "hover 没动效得点一下才有"。CSS 设计意图是 hover 在 un-focused 浮窗也
  // 应该工作（透明浮窗的鼠标反馈是用户唯一的交互指示），所以这个守卫直接
  // 违反设计意图。40ms debounce + 失焦时主动 setHoverAttr(false) + visibility
  // change 清理已经够防 spurious，focus check 多此一举。
  const setHoverAttr = (on: boolean) => {
    if (on) {
      if (document.body.dataset.hover === "1") return;
      document.body.dataset.hover = "1";
    } else {
      if (!("hover" in document.body.dataset)) return;
      delete document.body.dataset.hover;
      // hover 撤销 -> 清 Rust 路径同值去重状态，允许多次进入 hover
      lastHoverPayload = null;
    }
  };
  // 提前到 setHoverAttr 之前闭包共享，让 onFocusChanged 失焦时也能清 dedup
  let lastHoverPayload: boolean | null = null;
  // (a) 失焦时主动清 hover 状态：用户切到别 app 回来时不要"粘"住上次的 hover
  //     显形（避免 stale state）。**注意**：这里只清状态，**不**挡后续 hover
  //     —— un-focused 浮窗的合法 hover 必须能 toggle。
  //     失焦瞬间 WKWebView 偶发派发的 spurious enter 不会触发显形，因为：
  //     (1) 失焦时我们主动 setHoverAttr(false)，spurious enter 要走 40ms
  //         debounce 才会显形，40ms 内用户的真实操作会覆盖；
  //     (2) 失焦后 WKWebView 在 macOS 不向 un-focused webview 派 mouseMoved
  //         / mouseenter 事件，spurious enter 实际很少触发。
  let pageVisible = true;
  const wForFocus = getCurrentWindow();
  wForFocus
    .onFocusChanged(({ payload: f }) => {
      if (!f) setHoverAttr(false);
    })
    .catch(() => {});
  // 窗口 resize → 重新评估 narrow 档位（hint 字号 / 文字 / 容器 padding 联动）。
  // Tauri resize 事件在拖完边放手后派发一次（不是连发），不需要 debounce。
  // 启动时也调一次确保正确初始态（用户启动时窗口可能 = minWidth 240px）。
  wForFocus
    .onResized(() => {
      syncNarrowMode();
    })
    .catch(() => {});
  syncNarrowMode();
  // ── 全局快捷键 → 激活视觉（等价于 hover）+ 输入聚焦 ──
  //
  // 状态机（data-active）：
  //   启动：usticky://quick-add 事件触发（来自 Cmd+Shift+Space / tray 唤起）
  //   撤销：(1) 鼠标进入浮窗 → hover 接管（clearActive）
  //          (2) input.blur（Esc 或主动点别处）
  //          (3) visibility hidden（切到别 app）
  //          (4) 90s 自动 timeout（防"用户忘了浮窗在 active"长期占用资源）
  //
  // setActive/clearActive 提到此处，因为 visibilitychange listener
  // （见下方 line 712）在文件流中更早执行，需要 hoisting 兼容 —— 用
  // 函数声明而非 const arrow，避免 TDZ。
  let activeTimeoutHandle: ReturnType<typeof setTimeout> | null = null;
  const ACTIVE_TIMEOUT_MS = 90_000;
  function clearActive(): void {
    if (activeTimeoutHandle !== null) {
      clearTimeout(activeTimeoutHandle);
      activeTimeoutHandle = null;
    }
    if (!("active" in document.body.dataset)) return;
    delete document.body.dataset.active;
  }
  function setActive(): void {
    // 已经有 hover 在 → 视觉已经被接管，data-active 没必要叠
    if (document.body.dataset.hover === "1") return;
    if (document.body.dataset.active === "1") {
      // 重复触发 → 只重置 timeout，不重复触发视觉切换
      if (activeTimeoutHandle !== null) clearTimeout(activeTimeoutHandle);
      activeTimeoutHandle = setTimeout(clearActive, ACTIVE_TIMEOUT_MS);
      return;
    }
    document.body.dataset.active = "1";
    activeTimeoutHandle = setTimeout(clearActive, ACTIVE_TIMEOUT_MS);
  }

  document.addEventListener("visibilitychange", () => {
    pageVisible = document.visibilityState === "visible";
    if (!pageVisible) {
      setHoverAttr(false);
      // 隐藏时清 active —— 切别 app 后不该留"假激活"
      clearActive();
    }
  });
  // (b) 显形**不再** debounce —— 2026-07-08 fix：
  //     旧设计 enter→leave 40ms 阈值用于吞 WKWebView spurious enter。
  //     但 Rust 端 dwell-time hysteresis（enter 1 tick = 50ms 单 tick 验证，
  //     macos.rs ENTER_THRESHOLD=1）已经做了真正的边缘抖动去抖。
  //     JS 这层 40ms debounce 是**冗余的**，且把"WKWebView 派 mouseenter
  //     到 setHoverAttr 触发"的延迟拉高到 40ms+，是用户感知"hover 响应
  //     慢"的主因之一。Musage 用 40ms 是历史包袱，Usticky 不背。
  //
  //     副作用：WKWebView 偶发的 spurious mouseenter（macOS 上极少见）
  //     现在会被立即响应。但 spurious enter 通常伴随 spurious leave，hover
  //     状态机会在 1 帧内回到 false，CSS 220ms 过渡看不出来。实测无闪烁。
  let hoverEnterTimer: ReturnType<typeof setTimeout> | null = null;
  const onBodyMouseEnter = () => {
    if (!pageVisible) return; // 隐藏态 → spurious 忽略
    // hoverEnterTimer 现在基本是 null（debounce 已去除），保留 cancel 是
    // 防御性写法 —— 未来若 WKWebView 重新派 spurious mouseenter 触发回归，
    // 不用改这里。
    if (hoverEnterTimer !== null) {
      clearTimeout(hoverEnterTimer);
      hoverEnterTimer = null;
    }
    setHoverAttr(true);
  };
  const onBodyMouseLeave = () => {
    if (hoverEnterTimer !== null) {
      clearTimeout(hoverEnterTimer);
      hoverEnterTimer = null;
    }
    setHoverAttr(false);
  };
  document.body.addEventListener("mouseenter", onBodyMouseEnter);
  document.body.addEventListener("mouseleave", onBodyMouseLeave);

  // ── IPC: 监听 todos-changed 事件 ──
  let unlistenTodos: UnlistenFn | null = null;
  listen<TodoSnapshot>("usticky://todos-changed", (e) => {
    render(e.payload);
  })
    .then((fn) => (unlistenTodos = fn))
    .catch((e) => console.error("[usticky] listen todos-changed failed", e));

  // 后端 hover emitter 兜底（macOS / Win），与 JS mouseenter/leave 等效
  // (c) Rust emit 同值去重：避免 CSS spring 动画进行中反复重置起始点。
  //     Rust 端已有 dwell-time hysteresis，这里再做一层保险。
  //     **不挡 payload=true**：un-focused 浮窗的合法 hover 由 Rust 路径触发。
  //     失焦时 `onFocusChanged` 主动 setHoverAttr(false) 已清状态；visibility
  //     hidden 时 pageVisible=false 守卫挡掉。focus check 已被证明是 bug。

  // Rust 路径状态机：由 `usticky://floating-hover` 的 inside edge 驱动，
  // **不**依赖 `windowFocused`（PinBottom 默认 mode 下 macOS 不派 focus
  // change 事件，初值错了会让 Rust 路径永远短路）。
  let rustPathActive = false;
  let rustHoveredCardId: string | null = null;
  let unlistenHover: UnlistenFn | null = null;
  // **2026-07-03 fix（玻璃 2 秒后丢失问题）**：
  // 之前同值去重（`lastHoverPayload === e.payload`）是无时间窗口的永久短路 —— 如果
  // hover emitter 因 main thread 忙持续返 false 数十 tick，前端收到一次 false
  // 后 lastHoverPayload=false 锁死，之后即使 hover emitter 恢复返 true
  // （macOS 合成层重排期结束 ~1.5~3s 后），也会被 `lastHoverPayload === e.payload`
  // 永久短路 → body[data-hover] 永远清不掉。
  //
  // 修复：dedup 加 800ms TTL。800ms 内同 payload 直接吞（防 hover emitter
  // 边缘抖动反复 emit），超过 800ms 强制允许 emit，让 hover emitter 的真实
  // 状态变化穿透 dedup。
  //
  // 800ms = 16 ticks（50ms × 16）—— 比 macOS 合成层重排稳定时间（~1.5~3s）
  // 短，确保真恢复时能立即穿透；又比 hover emitter 边缘抖动窗口长，确保
  // 真抖动被吞。
  let lastHoverEmitAt = 0;
  const HOVER_DEDUP_TTL_MS = 800;
  listen<boolean>("usticky://floating-hover", (e) => {
    if (e.payload && !pageVisible) return;
    const now = performance.now();
    if (lastHoverPayload === e.payload && (now - lastHoverEmitAt) < HOVER_DEDUP_TTL_MS) {
      return;
    }
    lastHoverPayload = e.payload;
    lastHoverEmitAt = now;
    // Rust 路径直接同步切（已经过 50ms tick 去抖）。
    // hoverEnterTimer 现在基本是 null（debounce 已去除），保留 cancel 是
    // 防御性写法 —— 未来若 WKWebView 重新派 spurious mouseenter 触发回归，
    // 这里能立即切到 hover 状态不被 debounce 延迟。
    if (hoverEnterTimer !== null) {
      clearTimeout(hoverEnterTimer);
      hoverEnterTimer = null;
    }
    setHoverAttr(e.payload);
    // Rust 路径激活 / 收尾
    rustPathActive = e.payload;
    if (!e.payload && rustHoveredCardId !== null) {
      const oldId = rustHoveredCardId;
      rustHoveredCardId = null;
      const old = app.querySelector<HTMLElement>(`.todo-card[data-todo-id="${cssEscape(oldId)}"]`);
      if (old) scheduleHoverResize(old, false);
    }
  })
    .then((fn) => (unlistenHover = fn))
    .catch((e) => console.error("[usticky] listen floating-hover failed", e));

  // ── 未聚焦时 per-card hover：Rust 50ms tick emit 鼠标屏幕坐标 ──
  //
  // 聚焦时 WebView 自己派 mouseenter/leave，已经驱动 scheduleHoverResize。
  // 未聚焦时 WKWebView 不派 mouseenter，必须靠这条路径：拿到屏幕 logical
  // 坐标 → 转 viewport logical 坐标 → document.elementFromPoint → 命中
  // 哪张 .todo-card → scheduleHoverResize(true)。
  //
  // 跟 body[data-hover] 玻璃色的关系：
  //   - body[data-hover] 用 usticky://floating-hover（edge-trigger，每 tick 不发）
  //   - per-card title-expand 用 usticky://floating-hover-pos（inside=true 每 tick 发）
  //   两条独立 —— 玻璃色只看 in/out boundary，title-expand 要跟踪卡间切换。
  //
  // 频率 50ms × ~20Hz，每 tick 调一次 elementFromPoint。开销 ~微秒，
  // 不会触发任何 layout（elementFromPoint 走 hit test 不强制 reflow）。
  //
  // **激活判定**：由 `usticky://floating-hover` 的 inside edge 控制
  // `rustPathActive`（声明在前面的 listener 块），**不再依赖 `windowFocused`**
  // —— PinBottom 默认 mode 下浮窗被其他 app 盖住时 macOS 不派 focus change
  // 事件，初值 `true` 会让 Rust 路径永远短路。Rust emitter 50ms tick 自己
  // 判 topmost 是更准的"鼠标是否在浮窗未被遮挡区域"信号。
  let unlistenHoverPos: UnlistenFn | null = null;
  // 缓存浮窗位置 + scale。50ms × 20Hz 持续 hover-pos 时反复
  // `await innerPosition()` 是浪费 IPC，只在 onMoved 时刷新。
  let cachedWinX = 0;
  let cachedWinY = 0;
  let unlistenMoved: UnlistenFn | null = null;
  (async () => {
    try {
      const [pos, scale] = await Promise.all([
        wForFocus.innerPosition(),
        wForFocus.scaleFactor(),
      ]);
      cachedWinX = pos.x / scale;
      cachedWinY = pos.y / scale;
    } catch {
      /* fallback to 0/0 */
    }
  })();
  wForFocus
    .onMoved(() => {
      Promise.all([wForFocus.innerPosition(), wForFocus.scaleFactor()])
        .then(([pos, scale]) => {
          cachedWinX = pos.x / scale;
          cachedWinY = pos.y / scale;
        })
        .catch(() => {});
    })
    .then((fn) => (unlistenMoved = fn))
    .catch(() => {});

  listen<{ x: number; y: number }>("usticky://floating-hover-pos", (e) => {
    if (!rustPathActive) return;
    if (!pageVisible) return;
    const appEl = document.getElementById("app");
    if (!appEl) return;

    const { x: relX, y: relY } = screenToViewportCoords(
      e.payload.x,
      e.payload.y,
      cachedWinX,
      cachedWinY,
    );
    if (relX < 0 || relY < 0) {
      // Rust 报 inside=true 但坐标落在浮窗外（极少见，理论上 windowNumberAtPoint
      // 命中浮窗就一定在 frame 内），保守视为"没命中卡"。
      if (rustHoveredCardId !== null) {
        const oldId = rustHoveredCardId;
        rustHoveredCardId = null;
        const old = appEl.querySelector<HTMLElement>(
          `.todo-card[data-todo-id="${cssEscape(oldId)}"]`,
        );
        if (old) scheduleHoverResize(old, false);
      }
      return;
    }

    // elementFromPoint 走 hit-test 不强制 layout；viewport 坐标 = viewport-relative。
    const el = document.elementFromPoint(relX, relY);
    const card = el?.closest<HTMLElement>(".todo-card") ?? null;
    const newId = card?.dataset.todoId ?? null;
    if (newId === rustHoveredCardId) return; // 还在同一张卡上

    // 卡切换 / 卡→空白：收起上一张，展开新一张（如果有）
    if (rustHoveredCardId !== null) {
      const oldId = rustHoveredCardId;
      const old = appEl.querySelector<HTMLElement>(
        `.todo-card[data-todo-id="${cssEscape(oldId)}"]`,
      );
      if (old) scheduleHoverResize(old, false);
    }
    rustHoveredCardId = newId;
    if (card && newId) scheduleHoverResize(card, true);
  })
    .then((fn) => (unlistenHoverPos = fn))
    .catch((e) => console.error("[usticky] listen floating-hover-pos failed", e));

  // ── Backdrop refresh (L3 修复) ──
  // Rust 端在 set_window_level（level 切换）后 emit `usticky://backdrop-reflow`，
  // 前端给所有 .todo-card / .todo-input 加 .force-reflow class
  // (filter: drop-shadow 微动 → paint invalidation → backdrop layer 重采样)，
  // 击穿 macOS WKWebView ~2s 的 sample 失效窗口。
  //
  // 击穿机制：
  //   - level 切换 → WKWebView 旧 backdrop sample 失效
  //   - 通常 ~2s 后 WebKit 才重新计算 sample（用户看到"2 秒丢失"）
  //   - 我们在切换后立刻强制 paint invalidation，WebKit 必须立即重采样
  //   - .force-reflow class 持续 100ms 后移除（视觉几乎不可见）
  //
  // 注意：前端这个修复跟 Rust 的 hover emitter dispatch 抖动修复（4 层 那个）
  // 是**正交的**——前者解决 sample 失效（物理层），后者解决 hover 状态机
  // 误触发 CSS 切换（逻辑层）。两者一起叠，玻璃质感稳定。
  let backdropRefreshTimer: ReturnType<typeof setTimeout> | null = null;
  let unlistenBackdropRefresh: UnlistenFn | null = null;
  listen("usticky://backdrop-refresh", () => {
    if (backdropRefreshTimer !== null) clearTimeout(backdropRefreshTimer);
    const targets = app.querySelectorAll<HTMLElement>(".todo-card, .todo-input");
    targets.forEach((el) => el.classList.add("force-reflow"));
    backdropRefreshTimer = setTimeout(() => {
      backdropRefreshTimer = null;
      app.querySelectorAll<HTMLElement>(".todo-card.force-reflow, .todo-input.force-reflow")
        .forEach((el) => el.classList.remove("force-reflow"));
    }, 100);
  })
    .then((fn) => (unlistenBackdropRefresh = fn))
    .catch((e) => console.error("[usticky] listen backdrop-refresh failed", e));

  // ── 浮窗触底：resize_floating_window 检测到窗口高度被 clamp 到屏幕底部 ──
  //
  // 业务：当 todo 内容超过顶部 y 不动时的可用 height（commit 340605c 顶部钉死），
  // 浮窗 bottom 贴到屏幕底 + dock 上方 —— wheel 滚到底时最后一行 todo 卡常被
  // dock 栏盖住。Rust 端在 enter/exit bottomed 状态时 emit
  // `usticky://floating-bottomed { value: bool }`，前端设 body[data-bottomed]
  // 让 CSS 加 padding-bottom: 32px（styles.css line ~179），让最后一行滚到
  // dock 上方仍有 32px 喘息。
  //
  // enter/exit edge 触发：**不**每次 resize 都发，避免高频小幅 resize 让
  // 数据属性抖动引发 CSS 闪烁。前端单 listener 处理。
  let unlistenBottomed: UnlistenFn | null = null;
  listen<boolean>("usticky://floating-bottomed", (e) => {
    const next = e.payload ? "1" : "";
    if (document.body.dataset.bottomed === next) return;
    if (next) document.body.dataset.bottomed = next;
    else delete document.body.dataset.bottomed;
  })
    .then((fn) => (unlistenBottomed = fn))
    .catch((e) => console.error("[usticky] listen floating-bottomed failed", e));

  // ── 渲染输入区（常驻） ──
  // 在拉 quick_add_shortcut 之前调，确保 hint 元素存在；拉到 shortcut 后再 updateInputHint 刷
  ensureInputBar();

  // ── 启动时拉一次 quick-add 快捷键 —— input hint 用 ──
  try {
    currentShortcut = await invoke<string>("get_quick_add_shortcut");
    // 用当前档位刷（窗口可能已经窄到 tier 2，hint 文字应是 '⌘⇧'）
    updateInputHint(computeNarrowTier());
  } catch (e) {
    console.error("[usticky] get_quick_add_shortcut failed", e);
  }

  // ── 启动时拉一次 pin mode —— 必须在首次 render 之前完成，
  //    否则 foot 的 pin-ctrl 会用默认 pin_top 渲染一次再被覆盖（视觉闪烁）。
  let unlistenPinMode: UnlistenFn | null = null;
  try {
    currentPinMode = await invoke<PinMode>("get_pin_mode");
  } catch (e) {
    console.error("[usticky] get_pin_mode failed", e);
  }
  // sync 到 body[data-pin-mode] —— CSS 据此在 PinBottom idle 进一步淡化 done 卡
  document.body.dataset.pinMode = currentPinMode;

  // ── 启动时拉一次 snapshot ──
  try {
    const snap = await invoke<TodoSnapshot>("get_todos");
    render(snap);
  } catch (e) {
    console.error("[usticky] get_todos failed", e);
    renderEmptyState();
  }
  // pin mode 已稳定，绑定 hover-raise 监听（仅 PinBottom 模式实际挂）
  setupPinModeHoverRaise(currentPinMode);

  listen<PinMode>("usticky://pin-mode-changed", (e) => {
    if (e.payload !== currentPinMode) {
      currentPinMode = e.payload;
      document.body.dataset.pinMode = currentPinMode;
      setupPinModeHoverRaise(currentPinMode);
    }
  })
    .then((fn) => (unlistenPinMode = fn))
    .catch((e) => console.error("[usticky] listen pin-mode-changed failed", e));

  // ── quick-add 快捷键切换链路：设置面板改完后，浮窗 input hint 同步刷 ──
  let unlistenShortcut: UnlistenFn | null = null;
  listen<string>("usticky://shortcut-changed", (e) => {
    if (e.payload !== currentShortcut) {
      currentShortcut = e.payload;
      // 用当前档位刷（tier 2 时 hint 变 '⌘⇧'，不能用默认 0）
      updateInputHint(computeNarrowTier());
    }
  })
    .then((fn) => (unlistenShortcut = fn))
    .catch((e) => console.error("[usticky] listen shortcut-changed failed", e));

  // ── 浮窗拖动：左键 mousedown 但 target 是 .todo-card 或 button 时跳过 ──
  // 输入区（.todo-input / input）也允许拖窗，但要走阈值法：
  //   - 单击（mousedown→mouseup 无明显位移）→ 默认行为，聚焦 input 打字
  //   - 拖动（位移 > 5px）→ startDragging，把整个浮窗拖走
  // 否则输入框无法既当拖把又当输入框。AGENTS.md #11 的 .todo-row 冲突原则
  // 在此扩展为"input 既要聚焦又要拖窗"，用位移阈值区分意图。
  const w = getCurrentWindow();
  const DRAG_THRESHOLD = 5;
  app.addEventListener("mousedown", (e) => {
    if (e.button !== 0) return;
    const target = e.target as HTMLElement;
    if (target.closest(".todo-card, button")) return;
    const inInput = !!target.closest("input, .todo-input");
    if (!inInput) {
      e.preventDefault();
      w.startDragging().catch((err) => console.debug("[usticky] startDragging failed", err));
      return;
    }
    // 输入区：阈值法，避免抢走 click→focus
    const startX = e.clientX;
    const startY = e.clientY;
    let started = false;
    const onMove = (ev: MouseEvent) => {
      if (started) return;
      if (Math.abs(ev.clientX - startX) > DRAG_THRESHOLD ||
          Math.abs(ev.clientY - startY) > DRAG_THRESHOLD) {
        started = true;
        cleanup();
        e.preventDefault();
        w.startDragging().catch((err) => console.debug("[usticky] startDragging failed", err));
      }
    };
    const onUp = () => cleanup();
    const cleanup = () => {
      document.removeEventListener("mousemove", onMove);
      document.removeEventListener("mouseup", onUp);
    };
    document.addEventListener("mousemove", onMove);
    document.addEventListener("mouseup", onUp);
  });

  // ── 全局快捷键：监听后端 emit 的 quick-add 事件 → 激活视觉 + 聚焦 input ──
  //
  // 激活 vs hover：用户按 Cmd+Shift+Space 后浮窗是 macOS key window，
  // 但**鼠标不在浮窗上**，所以 hover emitter 不发浮窗内 → body[data-hover]
  // 不会被设、浮窗仍处于 idle 弱化态 → 视觉上"没看到自己刚刚唤起"的反馈。
  // 解决：quick-add 触发时设 body[data-active]=1（CSS 上跟 [data-hover]
  // 等价共享同一组 CSS variable），input.blur / 鼠标进入接管 / 90s 兜底
  // 时撤销。CSS 合并见 src/styles.css 的 body[data-hover], body[data-active]
  // 组合选择器。setActive/clearActive 在文件流中早就定义（见 line ~712
  // 之前的"激活视觉状态机"块）。
  let unlistenQuickAdd: UnlistenFn | null = null;
  listen("usticky://quick-add", () => {
    const input = app.querySelector<HTMLInputElement>(".todo-input input");
    if (input) {
      input.focus();
      input.select();
    }
    // 即便 input 不在（极端情况下空 todo-list 渲染前 emit），仍激活视觉，
    // 让浮窗先用全强度显示
    setActive();
  })
    .then((fn) => (unlistenQuickAdd = fn))
    .catch((e) => console.error("[usticky] listen quick-add failed", e));

  // input.blur 撤销 active —— 用户按 Esc / 主动点别处后浮窗退回 idle
  // 弱化态。注意：input.blur 也会在 window 失焦时触发（macOS 上切到别 app
  // 浮窗失焦 → input 自动 blur），这是我们要的：用户离开后 active 自动清。
  // 但 mouseenter 设的 data-hover 已经先发生，blur 撤销 active 后视觉
  // 仍由 data-hover 守住 —— 不冲突。
  const todoInputEl = app.querySelector<HTMLInputElement>(".todo-input input");
  if (todoInputEl) {
    todoInputEl.addEventListener("blur", () => {
      // 只在没 hover 时清 active —— 有 hover 的话视觉强度靠 data-hover
      if (document.body.dataset.hover !== "1") clearActive();
    });
  }
  // 鼠标进入 → 在原 onBodyMouseEnter 的 setHoverAttr(true) 后跟一行
  // clearActive()，避免双触发 + timeout 残留。onBodyMouseEnter 在文件流
  // 中更早定义（见 ~720 行），把它替换为新的活跃感知版（同样的 0ms debounce
  // 逻辑 —— 沿用 2026-07-08 fix 决策，去掉 40ms debounce 让响应即时）。
  document.body.removeEventListener("mouseenter", onBodyMouseEnter);
  document.body.addEventListener("mouseenter", () => {
    if (!pageVisible) return;
    if (hoverEnterTimer !== null) {
      clearTimeout(hoverEnterTimer);
      hoverEnterTimer = null;
    }
    setHoverAttr(true);
    // hover 接管后清 active —— 视觉由 hover 维持，避免双触发 + 防 timeout 残留
    clearActive();
  });

  // ── 全局 Cmd+Z 撤销：v0.2 实现，先占位 ──
  document.addEventListener("keydown", (e) => {
    if ((e.metaKey || e.ctrlKey) && e.key === "z") {
      // TODO: undo last action
      console.debug("[usticky] undo 暂未实现");
    }
  });

  // ── 事件代理：empty state CTA / due label click ──
  app.addEventListener("click", async (e) => {
    const target = e.target as HTMLElement;
    if (target.closest(".focus-input")) {
      const input = app.querySelector<HTMLInputElement>(".todo-input input");
      if (input) input.focus();
    }
  });

  // ── Wheel 滚动：手动转发 #app 的 overflow 滚动 ──
  //
  // 业务背景：当 todo 数过多 → #app.scrollHeight > 浮窗最大高度时，
  // resizeWindowToContent 把窗口 clamp 到 screenH - 80px，**但 #app 自己
  // 还保持自然高度**（max-height: 100%） → #app 内部 overflow-y: auto
  // 出现滚动条。理论上 WebView 应该把 wheel 事件自动转给 overflow:auto
  // 元素，但 macOS NSPanel（Tauri transparent borderless 用的）在非 key
  // 状态下不一定把 wheel 派给 WKWebView —— 用户实测 wheel 没反应。
  //
  // 这里手动拦 wheel：只有当 #app 真的可滚（scrollHeight > clientHeight）
  // 时才 preventDefault + 推 scrollTop，否则放过（让外层 / 浏览器 / 其它
  // 元素响应）。passive: false 是 preventDefault 强制要求。
  //
  // 节流：deltaY 累加到 RAF 上，避免高频 wheel 事件把 scrollTop 推得过快。
  // 体验上等价于原生滚动（也是 RAF-driven），但确保 JS 拦截生效。
  let pendingDeltaY = 0;
  let wheelRafHandle: number | null = null;
  const flushWheel = () => {
    wheelRafHandle = null;
    const appEl = document.getElementById("app");
    if (!appEl || pendingDeltaY === 0) {
      pendingDeltaY = 0;
      return;
    }
    const before = appEl.scrollTop;
    const maxScroll = appEl.scrollHeight - appEl.clientHeight;
    // clamp 到 [0, maxScroll]：超过停止位时把剩余 delta 丢进"被吸收"路径
    // （不传给外层，因为我们已经 preventDefault 了 —— 体感上等同于
    // "滚到顶/底后边滚动边阻力"）。
    appEl.scrollTop = Math.max(0, Math.min(maxScroll, before + pendingDeltaY));
    pendingDeltaY = 0;
  };
  app.addEventListener("wheel", (e) => {
    const appEl = document.getElementById("app");
    if (!appEl) return;
    const scrollable = appEl.scrollHeight - appEl.clientHeight;
    if (scrollable <= 0) return;  // 没溢出 → 不拦，放过 wheel
    // 横滚也放过 —— #app 不能横滚，浮窗本身能横滚更不该
    if (Math.abs(e.deltaX) > Math.abs(e.deltaY)) return;
    e.preventDefault();
    pendingDeltaY += e.deltaY;
    if (wheelRafHandle === null) {
      wheelRafHandle = requestAnimationFrame(flushWheel);
    }
  }, { passive: false });

  // ── beforeunload 清理 ──
  window.addEventListener("beforeunload", () => {
    unlistenTodos?.();
    unlistenQuickAdd?.();
    unlistenPinMode?.();
    unlistenLocale?.();
    unlistenHover?.();
    unlistenHoverPos?.();
    unlistenMoved?.();
    unlistenShortcut?.();
    unlistenBackdropRefresh?.();
    unlistenBottomed?.();
    cleanupSortables();
    // 摘掉 hover listener（setHoverAttr 路径，命名引用）
    document.body.removeEventListener("mouseenter", onBodyMouseEnter);
    document.body.removeEventListener("mouseleave", onBodyMouseLeave);
    // 摘掉 hover-raise listener（仅在 PinBottom 模式注册过，需要命名引用）
    document.body.removeEventListener("mouseenter", onPinBottomHoverEnter);
    document.body.removeEventListener("mouseleave", onPinBottomHoverLeave);
  });
}

/// PinBottom 模式挂 body mouseenter/mouseleave → 调 set_floating_hover_raise。
/// macOS / Win 上 tracker 已自行处理（commands 那边识别 PinBottom 后转发到 native），
/// 这里只是"前端兜底信号"，让后端知道前端看到 hover 了，跨平台一致。
///
/// 其它模式不挂监听，避免无意义 IPC。
function setupPinModeHoverRaise(mode: PinMode) {
  // 幂等：先摘再装
  document.body.removeEventListener("mouseenter", onPinBottomHoverEnter);
  document.body.removeEventListener("mouseleave", onPinBottomHoverLeave);
  if (mode !== "pin_bottom") return;
  // 跟 setHoverAttr 共享一个 target —— body 撑满整个浮窗（CSS margin:0 + bg:transparent），
  // 鼠标移出浮窗时 mouseleave 100% 触发。
  document.body.addEventListener("mouseenter", onPinBottomHoverEnter);
  document.body.addEventListener("mouseleave", onPinBottomHoverLeave);
}

function onPinBottomHoverEnter() {
  invoke("set_floating_hover_raise", { hovering: true }).catch((e) =>
    console.error(e),
  );
}
function onPinBottomHoverLeave() {
  invoke("set_floating_hover_raise", { hovering: false }).catch((e) =>
    console.error(e),
  );
}

function ensureInputBar() {
  if (app.querySelector(".todo-input")) return;
  const bar = document.createElement("div");
  bar.className = "todo-input";
  bar.innerHTML = `
    <input type="text" maxlength="280" autocomplete="off" spellcheck="false"
           placeholder="${escapeHtml(t("app.input.placeholder"))}" />
    <span class="todo-input-hint">${escapeHtml(formatShortcutForDisplay(currentShortcut))}</span>
  `;
  // 插到最前（render 会保留）
  app.insertBefore(bar, app.firstChild);

  const input = bar.querySelector<HTMLInputElement>("input")!;
  input.addEventListener("keydown", async (e) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      const v = input.value;
      // 校验通过后才清空 —— 失败时回填，保留用户输入
      const trimmed = v.trim();
      if (!trimmed) {
        showMiniFlash(t("app.error.empty_title"));
        return;
      }
      if (trimmed.length > 280) {
        showMiniFlash(t("app.error.too_long", { max: 280 }));
        return;
      }
      input.value = "";
      await addTodo(trimmed);
    } else if (e.key === "Escape") {
      input.blur();
      // 走 hide_floating_window 命令 —— 跟 quick-add 状态联动：
      // 若浮窗是 quick-add 唤起的，hide 时要还原 level + 切回原 app
      invoke("hide_floating_window").catch((e) => console.error("[usticky] hide_floating_window failed", e));
    }
  });
}

init();