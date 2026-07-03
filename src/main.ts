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
let currentPinMode: PinMode = "pin_top";  // 默认跟后端 PinMode::default() 对齐

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

  // foot
  updateFoot(snap);
  // 输入中禁止 autoResize —— scrollHeight 跳变会打断输入（AGENTS.md #18）
  if (!app.querySelector<HTMLInputElement>(".todo-input input")?.matches(":focus")) {
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

  // 插在 .todo-input 之后、.foot 之前；如果 foot 不存在就 append 到末尾
  const foot = app.querySelector<HTMLElement>(".foot");
  if (foot) {
    app.insertBefore(empty, foot);
  } else {
    app.appendChild(empty);
  }

  // 空态也要刷新 foot（count 文案 + pin-ctrl），否则从"5 项任务"切到空态文案陈旧
  updateFoot({ todos: [], fetched_at: null });
  if (!app.querySelector<HTMLInputElement>(".todo-input input")?.matches(":focus")) {
    void autoResizeWindowToContent();
  }
}

function updateFoot(snap: TodoSnapshot) {
  let foot = app.querySelector<HTMLElement>(".foot");
  const count = snap.todos.filter((x) => x.status === "pending").length;
  // 单复数：英文 1 task vs N tasks；中文一律用 app.count.other
  const text =
    count === 1
      ? t("app.count.one", { count })
      : t("app.count.other", { count });

  // pin mode 三档切换：紧凑的 segmented control，跟 .foot 共一行
  const pinCtrl = `
    <div class="pin-ctrl" data-pin="${currentPinMode}">
      <span class="pin-ctrl-label">${escapeHtml(t("app.pin.label"))}</span>
      <button class="pin-btn ${currentPinMode === "pin_top" ? "active" : ""}" data-pin-value="pin_top">${escapeHtml(t("app.pin.top"))}</button>
      <button class="pin-btn ${currentPinMode === "pin_bottom" ? "active" : ""}" data-pin-value="pin_bottom">${escapeHtml(t("app.pin.bottom"))}</button>
      <button class="pin-btn ${currentPinMode === "normal" ? "active" : ""}" data-pin-value="normal">${escapeHtml(t("app.pin.normal"))}</button>
    </div>
  `;

  if (foot) {
    foot.innerHTML = `<span class="foot-text">${escapeHtml(text)}</span>${pinCtrl}`;
  } else {
    foot = document.createElement("div");
    foot.className = "foot";
    foot.innerHTML = `<span class="foot-text">${escapeHtml(text)}</span>${pinCtrl}`;
    app.appendChild(foot);
  }
}

/// 只刷新 pin-ctrl 的 active 状态（避免整个 foot 重建）
function refreshPinCtrl() {
  const ctrl = app.querySelector<HTMLElement>(".pin-ctrl");
  if (!ctrl) return;
  ctrl.dataset.pin = currentPinMode;
  ctrl.querySelectorAll<HTMLElement>(".pin-btn").forEach((btn) => {
    btn.classList.toggle("active", btn.dataset.pinValue === currentPinMode);
  });
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
    refreshPinCtrl();  // pin-ctrl 文案要随 locale 变（"Top"/"Bottom"/"Normal" ↔ "置顶"/"置底"/"默认"）
    // input placeholder + hint 也需要随 locale 刷（创建时写死的）
    const input = app.querySelector<HTMLInputElement>(".todo-input input");
    if (input) input.placeholder = t("app.input.placeholder");
    const hint = app.querySelector<HTMLElement>(".todo-input-hint");
    if (hint) hint.textContent = t("app.input.shortcut_hint");
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

  // ── Hover attribute toggle ──
  //
  // 三个来源会触发 body[data-hover]：
  //   1. JS mouseenter/mouseleave（在 body 上）
  //   2. Rust hover emitter（50ms tick）emit usticky://floating-hover
  //   3. CSS :hover（被 body[data-hover] 自身覆盖）
  //
  // 鼠标在浮窗边缘抖动时，1 + 2 会反复翻转 → backdrop-filter 28px 反复
  // 重合成 → 视觉上"光标闪烁"。这里用 rAF 合并 + debounce，把多源
  // 抖动收敛成最多 1 次 / 帧的 DOM 写。
  let pendingHover: boolean | null = null;
  let hoverRafId: number | null = null;
  const setHoverAttr = (on: boolean) => {
    if (pendingHover === on) return; // 同帧重复写，跳过
    pendingHover = on;
    if (hoverRafId !== null) return;
    hoverRafId = requestAnimationFrame(() => {
      hoverRafId = null;
      const target = pendingHover!;
      pendingHover = null;
      if (target) document.body.dataset.hover = "1";
      else if (document.body.dataset.hover) delete document.body.dataset.hover;
    });
  };
  // macOS / Win 上非 key window CSS :hover 不生效 —— Rust hover emitter 是
  // 权威源；JS mouseenter/leave 留作 Linux 兜底（不依赖 Rust 实现）。
  document.body.addEventListener("mouseenter", () => setHoverAttr(true));
  document.body.addEventListener("mouseleave", () => setHoverAttr(false));

  // ── IPC: 监听 todos-changed 事件 ──
  let unlistenTodos: UnlistenFn | null = null;
  listen<TodoSnapshot>("usticky://todos-changed", (e) => {
    render(e.payload);
  })
    .then((fn) => (unlistenTodos = fn))
    .catch((e) => console.error("[usticky] listen todos-changed failed", e));

  // 后端 hover emitter 兜底（macOS / Win），与 JS mouseenter/leave 等效
  let unlistenHover: UnlistenFn | null = null;
  listen<boolean>("usticky://floating-hover", (e) => setHoverAttr(e.payload))
    .then((fn) => (unlistenHover = fn))
    .catch((e) => console.error("[usticky] listen floating-hover failed", e));

  // ── 渲染输入区（常驻） ──
  ensureInputBar();

  // ── 启动时拉一次 pin mode —— 必须在首次 render 之前完成，
  //    否则 foot 的 pin-ctrl 会用默认 pin_top 渲染一次再被覆盖（视觉闪烁）。
  let unlistenPinMode: UnlistenFn | null = null;
  try {
    currentPinMode = await invoke<PinMode>("get_pin_mode");
  } catch (e) {
    console.error("[usticky] get_pin_mode failed", e);
  }

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
      refreshPinCtrl();
      setupPinModeHoverRaise(currentPinMode);
    }
  })
    .then((fn) => (unlistenPinMode = fn))
    .catch((e) => console.error("[usticky] listen pin-mode-changed failed", e));

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

  // ── 事件代理：empty state CTA / due label click / pin mode 切换 ──
  app.addEventListener("click", async (e) => {
    const target = e.target as HTMLElement;
    if (target.closest(".focus-input")) {
      const input = app.querySelector<HTMLInputElement>(".todo-input input");
      if (input) input.focus();
    } else if (target.closest(".pin-btn")) {
      const btn = target.closest<HTMLElement>(".pin-btn");
      const newMode = btn?.dataset.pinValue as PinMode | undefined;
      if (newMode && newMode !== currentPinMode) {
        try {
          await invoke("set_pin_mode", { mode: newMode });
          // 后端会 emit usticky://pin-mode-changed，handler 在上面已经接好
        } catch (err) {
          console.error("[usticky] set_pin_mode failed", err);
        }
      }
    }
  });

  // ── beforeunload 清理 ──
  window.addEventListener("beforeunload", () => {
    unlistenTodos?.();
    unlistenQuickAdd?.();
    unlistenPinMode?.();
    unlistenLocale?.();
    unlistenHover?.();
    cleanupSortables();
    // 摘掉 hover-raise listener（仅在 PinBottom 模式注册过，需要命名引用）
    document.body.removeEventListener("mouseenter", onBodyHoverEnter);
    document.body.removeEventListener("mouseleave", onBodyHoverLeave);
  });
}

/// PinBottom 模式挂 body mouseenter/mouseleave → 调 set_floating_hover_raise。
/// macOS / Win 上 tracker 已自行处理（commands 那边识别 PinBottom 后转发到 native），
/// 这里只是"前端兜底信号"，让后端知道前端看到 hover 了，跨平台一致。
///
/// 其它模式不挂监听，避免无意义 IPC。
function setupPinModeHoverRaise(mode: PinMode) {
  // 幂等：先摘再装
  document.body.removeEventListener("mouseenter", onBodyHoverEnter);
  document.body.removeEventListener("mouseleave", onBodyHoverLeave);
  if (mode !== "pin_bottom") return;
  // 跟 setHoverAttr 共享一个 target —— body 撑满整个浮窗（CSS margin:0 + bg:transparent），
  // 鼠标移出浮窗时 mouseleave 100% 触发。
  document.body.addEventListener("mouseenter", onBodyHoverEnter);
  document.body.addEventListener("mouseleave", onBodyHoverLeave);
}

function onBodyHoverEnter() {
  invoke("set_floating_hover_raise", { hovering: true }).catch((e) =>
    console.error(e),
  );
}
function onBodyHoverLeave() {
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
    <span class="todo-input-hint">${escapeHtml(t("app.input.shortcut_hint"))}</span>
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
      win().hide().catch(() => {});
    }
  });
}

function win() {
  return getCurrentWindow();
}

init();