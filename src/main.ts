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
type NarrowTier = 0 | 1 | 2;

/// 浮窗宽度档位阈值（px）。
///
///  - tier 0（正常，≥ COMPACT_THRESHOLD）：hint = 10px + 背景框 + '⌘⇧Space'
///  - tier 1（紧凑，ICON_THRESHOLD ≤ w < COMPACT_THRESHOLD）：
///          hint = 9px + 无背景 + '⌘⇧Space'（省 ~14px）
///  - tier 2（图标，w < ICON_THRESHOLD）：
///          hint = 9px + 无背景 + '⌘⇧'（去掉 'Space' 文字，再省 ~20px）
///
/// ICON_THRESHOLD = minWidth 240：浮窗到最窄时 hint 必成图标版（否则必挤）。
/// COMPACT_THRESHOLD = 280：minWidth 240 + 40px 余量，给紧凑档提供触发空间。
const COMPACT_THRESHOLD = 280;
const ICON_THRESHOLD = 240;

/// 算当前宽度的档位。
function computeNarrowTier(): NarrowTier {
  const w = window.innerWidth;
  if (w < ICON_THRESHOLD) return 2;
  if (w < COMPACT_THRESHOLD) return 1;
  return 0;
}

/// 把 input hint 刷成 currentShortcut 的展示形式。
/// tier=2 时用图标版（`⌘⇧` 去 'Space'）省最宽档的字符。
/// tier=0/1 用完整版（`⌘⇧Space`）保信息密度。
function updateInputHint(tier: NarrowTier = 0) {
  const hint = app.querySelector<HTMLElement>(".todo-input-hint");
  if (hint) {
    hint.textContent = tier === 2
      ? formatShortcutIcon(currentShortcut)
      : formatShortcutForDisplay(currentShortcut);
  }
}

/// 根据 `window.innerWidth` 切档：设 `body[data-narrow]` 触发 CSS，
/// 同时调 `updateInputHint(tier)` 切文字（tier 2 时 hint 变 `⌘⇧`）。
///
/// 三档策略：
///   - 不用 display:none 隐藏 hint（用户期望"图标不被挤压"）
///   - 不到最窄不用 '⌘⇧' 简写（'⌘⇧Space' 才是完整信息）
///   - input 字号不变（用户已习惯 13px + 占位符宽度节奏）
function syncNarrowMode() {
  const tier = computeNarrowTier();
  document.body.dataset.narrow = tier === 0 ? "" : String(tier);
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

  // 检测当前是否在拖拽 —— 若在拖，destroy Sortable 会让 drag 中断。
  // 拖拽期间跳过一次重建，待 onEnd 后下一次 render 再统一刷新。
  const dragging = sortableInstances.some((s) => (s as any).dragEl != null);
  if (dragging) return;

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
    header.className = "section-header";
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

  // 挂 SortableJS
  const pendingSection = list.querySelector<HTMLElement>(".todo-section-pending");
  if (pendingSection) {
    sortableInstances.push(
      new Sortable(pendingSection, {
        animation: 150,
        ghostClass: "dragging",
        onEnd: handleDragEnd,
      }),
    );
  }
  const doneSection = list.querySelector<HTMLElement>(".todo-section-done");
  if (doneSection) {
    sortableInstances.push(
      new Sortable(doneSection, {
        animation: 150,
        ghostClass: "dragging",
        onEnd: handleDragEnd,
      }),
    );
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
  // 收集新顺序，批量提交
  const section = evt.to as HTMLElement;
  const ids: string[] = [];
  section.querySelectorAll<HTMLElement>(".todo-card").forEach((el) => {
    if (el.dataset.todoId) ids.push(el.dataset.todoId);
  });
  try {
    await invoke("reorder_todos", { ids });
  } catch (e) {
    console.error("[usticky] reorder_todos failed", e);
  }
}

function cleanupSortables() {
  for (const s of sortableInstances) s.destroy();
  sortableInstances = [];
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
  document.addEventListener("visibilitychange", () => {
    pageVisible = document.visibilityState === "visible";
    if (!pageVisible) setHoverAttr(false);
  });
  // (b) 显形 debounce：enter→leave < 40ms 视为抖动，不切 data-hover。
  //     显形方向 debounce（enter 后等 40ms 才设 hover），撤销方向照常立即。
  let hoverEnterTimer: ReturnType<typeof setTimeout> | null = null;
  const HOVER_DEBOUNCE_MS = 40;
  const onBodyMouseEnter = () => {
    if (!pageVisible) return; // 隐藏态 → spurious 忽略
    if (hoverEnterTimer !== null) clearTimeout(hoverEnterTimer);
    hoverEnterTimer = setTimeout(() => {
      hoverEnterTimer = null;
      setHoverAttr(true);
    }, HOVER_DEBOUNCE_MS);
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
  let unlistenHover: UnlistenFn | null = null;
  listen<boolean>("usticky://floating-hover", (e) => {
    if (e.payload && !pageVisible) return;
    if (lastHoverPayload === e.payload) return;
    lastHoverPayload = e.payload;
    // Rust 路径直接同步切（已经过 50ms tick 去抖）；cancel pending enter
    // timer（如果用户从 enter 进入但 40ms 内 Rust 也 emit true，按 Rust 为准）
    if (hoverEnterTimer !== null) {
      clearTimeout(hoverEnterTimer);
      hoverEnterTimer = null;
    }
    setHoverAttr(e.payload);
  })
    .then((fn) => (unlistenHover = fn))
    .catch((e) => console.error("[usticky] listen floating-hover failed", e));

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

  // ── 全局快捷键：监听后端 emit 的 quick-add 事件 → 聚焦 input ──
  let unlistenQuickAdd: UnlistenFn | null = null;
  listen("usticky://quick-add", () => {
    const input = app.querySelector<HTMLInputElement>(".todo-input input");
    if (input) {
      input.focus();
      input.select();
    }
  })
    .then((fn) => (unlistenQuickAdd = fn))
    .catch((e) => console.error("[usticky] listen quick-add failed", e));

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

  // ── beforeunload 清理 ──
  window.addEventListener("beforeunload", () => {
    unlistenTodos?.();
    unlistenQuickAdd?.();
    unlistenPinMode?.();
    unlistenLocale?.();
    unlistenHover?.();
    unlistenShortcut?.();
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