// QuickLook 式预览窗（v0.2）-- preview.html
//
// 两种形态（v0.2.x 起）：
//   1. hover 预览（label="preview"，URL 无 pinned）：hover 卡片 -> 弹出，
//      非聚焦面板，鼠标离开 / 浮窗 grace close 自关。pin 按钮 = 提升为
//      独立固定窗（调 pin_preview，后端建固定窗 + 关掉本 hover 预览）。
//   2. 固定窗（label="preview-pin-<todoId>"，URL ?pinned=1）：独立常驻，
//      blur / 浮窗 hide 都不自关，只走 Esc / 取消固定按钮（= 关窗）。
//      可同时存在多个（每个 todo 一个）。pin 按钮恒 active = 取消固定。
//
// 关闭路径：
//   hover 预览：Esc / blur / 浮窗 grace close / promoteToPinned 后端关 ->
//     closeSelf（emit preview-closed 让浮窗状态机复位）
//   固定窗：Esc / 取消固定按钮 -> closeSelf（**不** emit preview-closed --
//     独立窗口，不归浮窗 hover 状态机管）
//
// 编辑：textarea 输入防抖 700ms 自动保存（update_todo title）。空标题
// 不保存（后端 validate_title 会拒），hint 行短暂显示错误。
import { invoke, convertFileSrc } from "@tauri-apps/api/core";
import { emit, listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { t, initLocale, onLocaleChange, setLocale, getLocale } from "./i18n";
import { mark as perfMark, endMeasure as perfMeasureEnd } from "./perf";
import "./preview.css";

interface TodoAttachment {
  file: string;
  mime: string;
  width?: number | null;
  height?: number | null;
}

interface Todo {
  id: string;
  title: string;
  status: "pending" | "done";
  created_at: number;  // epoch ms
  updated_at: number;  // done 任务视为完成时间（翻 status 时后端刷新）
  attachment?: TodoAttachment | null;
}

// 底部操作按钮图标（复制 / 垃圾桶删除 -- v0.2.4 起全 App 统一垃圾桶，
// 与 main.ts 卡内按钮同 lucide trash-2 / feather copy 风格，13px 显示）
const COPY_ICON_SVG = `<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>`;
const TRASH_ICON_SVG = `<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M3 6h18"/><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6"/><path d="M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"/><line x1="10" y1="11" x2="10" y2="17"/><line x1="14" y1="11" x2="14" y2="17"/></svg>`;
/// pin 按钮（复制键左侧）。hover 预览里点 = 提升为独立固定窗（promoteToPinned）；
/// 固定窗里恒 active，点 = 取消固定（关窗）。lucide pin 图标（针头朝下）。
const PIN_ICON_SVG = `<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><line x1="12" y1="17" x2="12" y2="22"/><path d="M5 17h14v-1.76a2 2 0 0 0-1.11-1.79l-1.78-.9A2 2 0 0 1 15 10.76V6h1a2 2 0 0 0 0-4H8a2 2 0 0 0 0 4h1v4.76a2 2 0 0 1-1.11 1.79l-1.78.9A2 2 0 0 0 5 15.24Z"/></svg>`;

/// epoch ms -> 短日期（"2026/8/5" / "8/5/2026"，随 locale）
function formatDate(ms: number): string {
  return new Date(ms).toLocaleDateString(
    getLocale() === "zh-CN" ? "zh-CN" : "en-US",
    { year: "numeric", month: "numeric", day: "numeric" },
  );
}

interface TodoSnapshot {
  todos: Todo[];
}

const appEl = document.getElementById("preview-app")!;
const win = getCurrentWindow();

/// 本窗是否为「独立固定窗」模式（URL ?pinned=1）。hover 预览=false，
/// 固定窗=true。决定 pin 按钮语义 / blur 是否自关 / 是否广播浮窗 hover 事件。
const isPinned = new URLSearchParams(window.location.search).get("pinned") === "1";

let currentTodoId: string | null = null;
let currentTodo: Todo | null = null;
let attachmentsDir: string | null = null;
let saveTimer: ReturnType<typeof setTimeout> | null = null;
let closing = false;
const SAVE_DEBOUNCE_MS = 700;

/// hint 行的"静止文案"：固定窗显示「已固定 · Esc 关闭」提示如何释放，
/// hover 预览回默认 hint。flashHint 反馈完仍回到这里。
function hintRestingText(): string {
  return isPinned ? t("preview.hint_pinned") : t("preview.hint");
}

function attachmentUrl(file: string): string | null {
  if (!attachmentsDir) return null;
  const sep = attachmentsDir.endsWith("/") || attachmentsDir.endsWith("\\") ? "" : "/";
  return convertFileSrc(attachmentsDir + sep + file);
}

// ── 渲染 ──

/// S2：模块级骨架 refs。
let skeletonBuilt = false;
let panelEl: HTMLElement | null = null;
let imageEl: HTMLImageElement | null = null;
let textareaEl: HTMLTextAreaElement | null = null;
let createdEl: HTMLElement | null = null;
let hintEl: HTMLElement | null = null;
let actionsEl: HTMLElement | null = null;
let pinBtnEl: HTMLButtonElement | null = null;
let copyBtnEl: HTMLButtonElement | null = null;
let delBtnEl: HTMLButtonElement | null = null;
let doneAtEl: HTMLElement | null = null;

function ensureSkeleton() {
  if (skeletonBuilt) return;
  const panel = document.createElement("div");
  panel.className = "preview-panel";
  panelEl = panel;
  const textarea = document.createElement("textarea");
  textarea.className = "preview-text";
  textarea.spellcheck = false;
  textarea.maxLength = 114514;
  textareaEl = textarea;
  panel.appendChild(textarea);
  const footer = document.createElement("div");
  footer.className = "preview-footer";
  const created = document.createElement("span");
  created.className = "preview-date";
  createdEl = created;
  footer.appendChild(created);
  const hint = document.createElement("div");
  hint.className = "preview-hint";
  hintEl = hint;
  footer.appendChild(hint);
  const actions = document.createElement("div");
  actions.className = "preview-actions";
  actionsEl = actions;
  const pinBtn = document.createElement("button");
  pinBtn.className = "preview-action-btn preview-pin";
  pinBtn.innerHTML = PIN_ICON_SVG;
  pinBtnEl = pinBtn;
  actions.appendChild(pinBtn);
  const copyBtn = document.createElement("button");
  copyBtn.className = "preview-action-btn";
  copyBtn.innerHTML = COPY_ICON_SVG;
  copyBtnEl = copyBtn;
  actions.appendChild(copyBtn);
  const delBtn = document.createElement("button");
  delBtn.className = "preview-action-btn preview-delete";
  delBtn.innerHTML = TRASH_ICON_SVG;
  delBtnEl = delBtn;
  actions.appendChild(delBtn);
  footer.appendChild(actions);
  panel.appendChild(footer);
  appEl.appendChild(panel);
  textarea.addEventListener("input", () => {
    if (saveTimer) clearTimeout(saveTimer);
    saveTimer = setTimeout(() => {
      saveTimer = null;
      void saveTitle(textarea.value, hintEl);
    }, SAVE_DEBOUNCE_MS);
  });
  textarea.addEventListener("focus", () => {
    if (!isPinned && currentTodoId) {
      emit("usticky://preview-editing", { id: currentTodoId }).catch(() => {});
    }
  });
  panel.addEventListener("mousedown", (e) => {
    if (e.button !== 0) return;
    const target = e.target as HTMLElement;
    if (target.closest(".preview-text")) return;
    e.preventDefault();
    win.startDragging().catch((err) =>
      console.debug("[preview] startDragging failed", err),
    );
  });
  pinBtn.addEventListener("click", (e) => {
    e.stopPropagation();
    if (isPinned) closeSelf();
    else void promoteToPinned();
  });
  copyBtn.addEventListener("click", (e) => {
    e.stopPropagation();
    void copyTitle(hintEl);
  });
  delBtn.addEventListener("click", (e) => {
    e.stopPropagation();
    if (delBtn.dataset.confirm === "1") {
      void deleteSelf();
    } else {
      delBtn.dataset.confirm = "1";
      setTimeout(() => delete delBtn.dataset.confirm, 3000);
    }
  });
  skeletonBuilt = true;
}

function render(todo: Todo) {
  currentTodo = todo;
  ensureSkeleton();
  if (todo.attachment) {
    if (!imageEl || !imageEl.isConnected) {
      const img = document.createElement("img");
      img.className = "preview-image";
      img.alt = todo.title;
      img.draggable = false;
      img.addEventListener("error", () => {
        img.remove();
        imageEl = null;
      });
      panelEl!.insertBefore(img, textareaEl);
      imageEl = img;
    }
    const url = attachmentUrl(todo.attachment.file);
    if (url && imageEl.getAttribute("src") !== url) imageEl.src = url;
    if (imageEl.alt !== todo.title) imageEl.alt = todo.title;
  } else if (imageEl) {
    imageEl.remove();
    imageEl = null;
  }
  if (textareaEl) {
    const focused = document.activeElement === textareaEl;
    if (!focused && textareaEl.value !== todo.title) {
      textareaEl.value = todo.title;
    }
  }
  if (createdEl) {
    createdEl.textContent = `${t("preview.created")} ${formatDate(todo.created_at)}`;
  }
  if (hintEl) {
    const txt = hintEl.textContent ?? "";
    if (txt === "" || txt === hintRestingText()) {
      hintEl.textContent = hintRestingText();
    }
  }
  if (todo.status === "done") {
    if (!doneAtEl) {
      const d = document.createElement("span");
      d.className = "preview-date";
      doneAtEl = d;
      actionsEl!.insertBefore(d, pinBtnEl!);
    }
    doneAtEl.textContent = `${t("preview.completed")} ${formatDate(todo.updated_at)}`;
  } else if (doneAtEl) {
    doneAtEl.remove();
    doneAtEl = null;
  }
  if (pinBtnEl) {
    if (isPinned) {
      pinBtnEl.classList.add("active");
      pinBtnEl.setAttribute("aria-label", t("app.action.unpin"));
    } else {
      pinBtnEl.classList.remove("active");
      pinBtnEl.setAttribute("aria-label", t("app.action.pin"));
    }
  }
  if (copyBtnEl) copyBtnEl.setAttribute("aria-label", t("app.action.copy"));
  if (delBtnEl) delBtnEl.setAttribute("aria-label", t("app.action.delete"));
}

function renderMissing() {
  skeletonBuilt = false;
  imageEl = null;
  textareaEl = null;
  createdEl = null;
  hintEl = null;
  actionsEl = null;
  pinBtnEl = null;
  copyBtnEl = null;
  delBtnEl = null;
  doneAtEl = null;
  panelEl = null;
  appEl.innerHTML = "";
  const panel = document.createElement("div");
  panel.className = "preview-panel";
  const msg = document.createElement("div");
  msg.className = "preview-missing";
  msg.textContent = t("preview.missing");
  panel.appendChild(msg);
  appEl.appendChild(panel);
}

async function saveTitle(value: string, hint?: HTMLElement | null) {
  const id = currentTodoId;
  if (!id) return;
  const trimmed = value.trim();
  // 空标题 / 未改动 -> 不保存（后端 validate_title 拒空串，改了也白改）
  if (!trimmed || trimmed === currentTodo?.title) return;
  try {
    await invoke("update_todo", { id, title: trimmed });
    if (currentTodo) currentTodo.title = trimmed;
  } catch (e) {
    console.error("[preview] save title failed", e);
    const h = hint ?? appEl.querySelector<HTMLElement>(".preview-hint");
    if (h) {
      h.classList.add("error");
      h.textContent = t("app.error.save_failed");
      setTimeout(() => {
        h.classList.remove("error");
        h.textContent = hintRestingText();
      }, 2000);
    }
  }
}

/// 切 todo / 关窗前把 debounce 中未落盘的编辑 flush 掉。
function flushPendingSave() {
  if (saveTimer) {
    clearTimeout(saveTimer);
    saveTimer = null;
    const textarea = appEl.querySelector<HTMLTextAreaElement>(".preview-text");
    if (textarea) void saveTitle(textarea.value);
  }
}

// ── footer 按钮动作（v0.2.4） ──

/// hint 行短暂换文案做反馈（mini-flash 的预览窗等价物）。
function flashHint(hint: HTMLElement | null, msg: string) {
  const h = hint ?? appEl.querySelector<HTMLElement>(".preview-hint");
  if (!h) return;
  h.textContent = msg;
  setTimeout(() => {
    h.textContent = hintRestingText();
  }, 1500);
}

/// 复制标题全文：navigator.clipboard 优先，execCommand 兜底（非聚焦
/// webview / 权限受限时）。同 main.ts copyTodoText 双路径。
async function copyTitle(hint: HTMLElement | null) {
  const text = currentTodo?.title ?? "";
  if (!text) return;
  try {
    await navigator.clipboard.writeText(text);
  } catch (e) {
    console.debug("[preview] navigator.clipboard 失败，走 execCommand 兜底", e);
    const ta = document.createElement("textarea");
    ta.value = text;
    ta.style.cssText = "position:fixed;opacity:0;pointer-events:none;";
    document.body.appendChild(ta);
    ta.select();
    try {
      document.execCommand("copy");
    } catch (e2) {
      console.error("[preview] execCommand copy 也失败", e2);
    }
    ta.remove();
  }
  flashHint(hint, t("app.copy.flash"));
}

/// 删除当前 todo：后端 emit todos-changed -> 当前 id 没了 -> closeSelf
/// （listener 已有该路径），这里再直接 closeSelf 一次做幂等双保险。
async function deleteSelf() {
  const id = currentTodoId;
  if (!id) return;
  try {
    await invoke("delete_todo", { id });
    closeSelf();
  } catch (e) {
    console.error("[preview] delete_todo failed", e);
  }
}

/// hover 预览的 pin 按钮：把当前 todo 提升为独立固定窗。后端 pin_preview
/// 会创建 preview-pin-<todoId> 窗口（沿用本窗位置/尺寸，平滑过渡）并关掉
/// 本 hover 预览。本 webview 随即销毁，下面的 closeSelf 兜底未必跑到。
async function promoteToPinned() {
  const id = currentTodoId;
  if (!id) return;
  flushPendingSave();
  try {
    await invoke("pin_preview", { todoId: id });
    closeSelf();
  } catch (e) {
    console.error("[preview] pin_preview failed", e);
  }
}

// ── 关闭 ──

function closeSelf() {
  if (closing) return;
  closing = true;
  flushPendingSave();
  // 固定窗是独立的 -- 关掉不广播 preview-closed（那是浮窗 hover 状态机的
  // 复位信号，固定窗不归它管）。hover 预览关掉要广播让浮窗复位。
  if (!isPinned) {
    emit("usticky://preview-closed", {}).catch(() => {});
  }
  // win.close 失败时复位 closing + 走兜底 -- 否则 closing 永远 true，后续
  // Esc / blur 全被 `if (closing) return` 吞掉，窗口卡死常驻（v0.2.4 实测）。
  // hover 预览兜底走 close_preview_window（关 label="preview"）；固定窗兜底
  // 走 close_pinned_preview（按 todoId 关 preview-pin-<id>）。
  win.close().catch((e) => {
    console.debug("[preview] close failed, 走兜底", e);
    closing = false;
    if (isPinned) {
      // 固定窗 win.close 失败 -> 后端按 label 关（Rust w.close 不经 webview 权限）。
      if (currentTodoId) {
        invoke("close_pinned_preview", { todoId: currentTodoId }).catch((e2) =>
          console.debug("[preview] backend pinned close also failed", e2),
        );
      } else {
        win.close().catch(() => {});
      }
    } else {
      invoke("close_preview_window", { force: true }).catch((e2) =>
        console.debug("[preview] backend force close also failed", e2),
      );
    }
  });
}

// ── 加载指定 todo ──

async function loadTodo(id: string) {
  perfMark("preview-load-start");
  try {
    const snap = await invoke<TodoSnapshot>("get_todos");
    perfMark("preview-get-todos-end");
    perfMeasureEnd("preview-get-todos", "preview-load-start", "preview-get-todos-end");
    const todo = snap.todos.find((x) => x.id === id);
    if (!todo) {
      renderMissing();
      // todo 被删 -> 窗口没有存在意义
      closeSelf();
      return;
    }
    currentTodoId = id;
    perfMark("preview-render-start");
    render(todo);
    perfMark("preview-render-end");
    perfMeasureEnd("preview-render", "preview-render-start", "preview-render-end");
    perfMeasureEnd("preview-load-total", "preview-load-start");
  } catch (e) {
    console.error("[preview] get_todos failed", e);
    renderMissing();
  }
}

// ── 启动 ──

async function init() {
  await initLocale();
  document.title = t("preview.title");

  onLocaleChange(() => {
    document.title = t("preview.title");
    const hint = appEl.querySelector<HTMLElement>(".preview-hint");
    if (hint) hint.textContent = hintRestingText();
    // pin 按钮 aria-label 随语言刷新
    const pinBtn = appEl.querySelector<HTMLElement>(".preview-pin");
    if (pinBtn) {
      pinBtn.setAttribute("aria-label", t(isPinned ? "app.action.unpin" : "app.action.pin"));
    }
  });

  let unlistenLocale: UnlistenFn | null = null;
  listen<string>("usticky://locale-changed", async (e) => {
    if ((e.payload === "en" || e.payload === "zh-CN") && e.payload !== getLocale()) {
      await setLocale(e.payload);
    }
  })
    .then((fn) => (unlistenLocale = fn))
    .catch((e) => console.error("[preview] listen locale-changed failed", e));

  try {
    attachmentsDir = await invoke<string>("get_attachments_dir");
  } catch (e) {
    console.error("[preview] get_attachments_dir failed", e);
  }

  // 初始 todo：URL ?id=<uuid>
  const params = new URLSearchParams(window.location.search);
  const initialId = params.get("id");
  if (initialId) {
    await loadTodo(initialId);
  }
  // else: prewarm 创建的隐藏窗（URL 无 ?id=）-- **不**渲染 missing。
  // prewarm 时窗隐藏无所谓，但 reuse 时 Rust 先 w.show() 后 emit
  // preview-todo，show 到 loadTodo 完成（异步 IPC 往返）之间会闪现
  // "该任务已不存在"。留空 appEl（窗 transparent）让闪现变透明 -> 内容，
  // 不再误报 missing。真正的 missing 由 loadTodo 找不到 todo 时自己渲染。

  // 后端复用 hover 预览窗：emit 换 todo（hover 在卡间移动时复用同一预览窗）。
  // 固定窗 label 不同，收不到这个 emit（w.emit 只发给 "preview"），这里加
  // isPinned 守卫只是防御。
  let unlistenSetTodo: UnlistenFn | null = null;
  listen<{ id: string }>("usticky://preview-todo", (e) => {
    if (isPinned) return;
    if (e.payload.id && e.payload.id !== currentTodoId) {
      flushPendingSave();
      void loadTodo(e.payload.id);
    }
  })
    .then((fn) => (unlistenSetTodo = fn))
    .catch((e) => console.error("[preview] listen preview-todo failed", e));

  // 外部数据变化：当前 todo 被删 -> 关窗；标题被别处改 -> 未聚焦时回填。
  let unlistenTodos: UnlistenFn | null = null;
  listen<TodoSnapshot>("usticky://todos-changed", (e) => {
    if (!currentTodoId) return;
    const todo = e.payload.todos.find((x) => x.id === currentTodoId);
    if (!todo) {
      closeSelf();
      return;
    }
    currentTodo = todo;
    const textarea = appEl.querySelector<HTMLTextAreaElement>(".preview-text");
    if (textarea && !textarea.matches(":focus") && textarea.value !== todo.title) {
      textarea.value = todo.title;
    }
  })
    .then((fn) => (unlistenTodos = fn))
    .catch((e) => console.error("[preview] listen todos-changed failed", e));

  // Esc 关闭（QuickLook 语义）。textarea 里按 Esc 也关 -- 编辑有自动保存，
  // 不需要 Esc = 取消编辑的二级语义。固定窗同样走 Esc 关窗。
  window.addEventListener("keydown", (e) => {
    if (e.key === "Escape") {
      e.preventDefault();
      closeSelf();
    }
  });

  // 窗口 blur 关闭（仅 hover 预览）：用户聚焦过预览窗后点别处 -> 收。
  // focused(false) 创建的 hover 面板不会收到这个事件（状态没变化过）。
  // focused(true) -> emit preview-focused：浮窗据**只 cancelPreviewClose**
  // （防自动关窗），**不设 previewPinnedId**。窗口聚焦 ≠ 编辑 —— 点缩略图
  // 看图也抢焦点，若据此 pin 则 hover 又被锁死。编辑锁改由 textarea focus
  // （preview-editing 事件）显式设。blur 必触发 closeSelf -> preview-closed
  // 释放，不存在锁死路径。
  //
  // 固定窗：blur 不关（常驻），也不广播 preview-focused -- 它独立于浮窗
  // hover 状态机，不该挡 hover 换卡。
  let unlistenFocus: UnlistenFn | null = null;
  win
    .onFocusChanged(({ payload: focused }) => {
      if (isPinned) return;
      if (!focused) {
        closeSelf();
      } else if (currentTodoId) {
        emit("usticky://preview-focused", { id: currentTodoId }).catch(() => {});
      }
    })
    .then((fn) => (unlistenFocus = fn))
    .catch((e) => console.error("[preview] onFocusChanged failed", e));

  // 鼠标进出 -> 浮窗 pinned 状态机（仅 hover 预览）。固定窗不参与 -- 它独立，
  // 鼠标进/出不该触发浮窗 grace close 重排。
  document.body.addEventListener("mouseenter", () => {
    if (isPinned || !currentTodoId) return;
    emit("usticky://preview-entered", { id: currentTodoId }).catch(() => {});
  });
  document.body.addEventListener("mouseleave", () => {
    if (isPinned || !currentTodoId) return;
    // 聚焦中的预览窗（编辑态）离开鼠标不解除 pinned -- blur 会负责关。
    win
      .isFocused()
      .then((focused) => {
        if (!focused && currentTodoId) {
          emit("usticky://preview-left", { id: currentTodoId }).catch(() => {});
        }
      })
      .catch(() => {});
  });

  // 后端主动 close（浮窗 grace close / 浮窗 hide 兜底）：hover 预览补发
  // preview-closed 让浮窗状态机复位（浮窗自己发起的路径已自清，这里幂等双保险）。
  // 固定窗不会被浮窗 hide 收掉（hide_dismiss 只关 label="preview"），故不补发。
  window.addEventListener("beforeunload", () => {
    flushPendingSave();
    if (!isPinned) {
      emit("usticky://preview-closed", {}).catch(() => {});
    }
    unlistenLocale?.();
    unlistenSetTodo?.();
    unlistenTodos?.();
    unlistenFocus?.();
  });

  // prewarm 竞态防线（仅 hover 预览）：prewarm 隐藏创建的 webview 可能还没
  // 加载完，后端 open_preview_window reuse 路径 emit 的 preview-todo 会丢 ->
  // 后端在 emit 前先存 pending id，这里 listeners 就位后主动取一次。
  // 固定窗不走 prewarm 路径，跳过（否则会偷走 hover 预览的 pending id）。
  if (!isPinned) {
    try {
      const pendingId = await invoke<string | null>("take_pending_preview_todo");
      if (pendingId && pendingId !== currentTodoId) {
        await loadTodo(pendingId);
      }
    } catch (e) {
      console.debug("[preview] take_pending_preview_todo failed", e);
    }
  }
}

init();
