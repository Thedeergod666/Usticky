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
  // 远期或精度更高
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

  // 清掉旧内容（保留 input）
  cleanupSortables();
  const oldList = app.querySelector<HTMLElement>(".todo-list");
  if (oldList) oldList.remove();

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

  // foot
  updateFoot(snap);
  void autoResizeWindow(snap);
}

function buildTodoRow(todo: Todo): HTMLElement {
  const row = document.createElement("div");
  row.className = `todo-card${todo.status === "done" ? " done" : ""}`;
  row.dataset.todoId = todo.id;
  row.dataset.priority = todo.priority;
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
  app.innerHTML = `
    <div class="empty-state">
      <div class="empty-state-title">${escapeHtml(t("app.empty.title"))}</div>
      <div class="empty-state-subtitle">${escapeHtml(t("app.empty.subtitle"))}</div>
      <button class="empty-state-cta focus-input">${escapeHtml(t("app.empty.cta"))}</button>
    </div>
  `;
  void autoResizeWindowToContent();
}

function updateFoot(snap: TodoSnapshot) {
  let foot = app.querySelector<HTMLElement>(".foot");
  const count = snap.todos.filter((x) => x.status === "pending").length;
  const text = t("app.count.other", { count });
  if (foot) {
    foot.textContent = text;
  } else {
    foot = document.createElement("div");
    foot.className = "foot";
    foot.textContent = text;
    foot.style.fontSize = "10px";
    foot.style.color = "var(--text-faint)";
    foot.style.padding = "4px 6px 2px";
    foot.style.textAlign = "center";
    foot.style.textShadow = "var(--text-shadow)";
    app.appendChild(foot);
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
  // 乐观更新 DOM：先加 .vanishing 动画，动画结束后再调 IPC
  const row = app.querySelector<HTMLElement>(`.todo-card[data-todo-id="${cssEscape(todo.id)}"]`);
  if (todo.status === "pending") {
    // 标完成
    if (row) {
      row.classList.add("vanishing");
      setTimeout(async () => {
        try {
          await invoke("update_todo", { id: todo.id, status: "done" });
        } catch (e) {
          console.error("[usticky] update_todo failed", e);
          row.classList.remove("vanishing");
        }
      }, 300);
    } else {
      await invoke("update_todo", { id: todo.id, status: "done" });
    }
  } else {
    // 撤销完成
    try {
      await invoke("update_todo", { id: todo.id, status: "pending" });
      showMiniFlash(t("app.undo.flash", { title: todo.title }));
    } catch (e) {
      console.error("[usticky] undo failed", e);
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
    }, 300);
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
    if (lastRenderedSnap) render(lastRenderedSnap);
  });

  listen<string>("usticky://locale-changed", async (e) => {
    const newLocale = e.payload;
    if (newLocale === "en" || newLocale === "zh-CN") {
      if (newLocale !== getLocale()) await setLocale(newLocale);
    }
  });

  // ── Hover attribute toggle ──
  const setHoverAttr = (on: boolean) => {
    if (on) document.body.dataset.hover = "1";
    else delete document.body.dataset.hover;
  };
  document.body.addEventListener("mouseenter", () => setHoverAttr(true));
  document.body.addEventListener("mouseleave", () => setHoverAttr(false));

  // ── IPC: 监听 todos-changed 事件 ──
  let unlistenTodos: UnlistenFn | null = null;
  listen<TodoSnapshot>("usticky://todos-changed", (e) => {
    render(e.payload);
  })
    .then((fn) => (unlistenTodos = fn))
    .catch((e) => console.error("[usticky] listen todos-changed failed", e));

  // ── 渲染输入区（常驻） ──
  ensureInputBar();

  // ── 启动时拉一次 snapshot ──
  try {
    const snap = await invoke<TodoSnapshot>("get_todos");
    render(snap);
  } catch (e) {
    console.error("[usticky] get_todos failed", e);
    renderEmptyState();
  }

  // ── 浮窗拖动：左键 mousedown 但 target 是 .todo-card 或 input/button 时跳过 ──
  const w = getCurrentWindow();
  app.addEventListener("mousedown", (e) => {
    if (e.button !== 0) return;
    const target = e.target as HTMLElement;
    if (target.closest(".todo-card, input, button, .todo-input")) return;
    e.preventDefault();
    w.startDragging().catch((err) => console.debug("[usticky] startDragging failed", err));
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
  app.addEventListener("click", (e) => {
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
    cleanupSortables();
  });
}

function ensureInputBar() {
  if (app.querySelector(".todo-input")) return;
  const bar = document.createElement("div");
  bar.className = "todo-input";
  bar.innerHTML = `
    <input type="text" maxlength="280" autocomplete="off" spellcheck="false"
           placeholder="${escapeHtml(t("app.input.placeholder"))}" />
    <span class="todo-input-hint">${escapeHtml(t("app.input.shortcut_hint"))}</span>
  `;
  // 插到最前（render 会保留）
  app.insertBefore(bar, app.firstChild);

  const input = bar.querySelector<HTMLInputElement>("input")!;
  input.addEventListener("keydown", async (e) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      const v = input.value;
      input.value = "";
      await addTodo(v);
    } else if (e.key === "Escape") {
      input.blur();
      w().hide().catch(() => {});
    }
  });
}

function w() {
  return getCurrentWindow();
}

init();