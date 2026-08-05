// QuickLook 式预览窗（v0.2）—— preview.html
//
// 生命周期（跟浮窗 main.ts 的预览状态机配对）：
//   打开：open_preview_window 命令（hover dwell → focused(false) 面板；
//         点击缩略图 → focused(true) 编辑态）。已开时后端复用窗口 + emit
//         usticky://preview-todo 换内容，本文件原地 re-render。
//   关闭（三条路径，殊途同归）：
//     1. Esc 键 → closeSelf()
//     2. 窗口 blur（聚焦后用户点别处）→ closeSelf()
//     3. 浮窗侧 grace close / 浮窗 hide → 后端直接 close 本窗
//        （beforeunload 里补发 preview-closed，浮窗状态机幂等清理）
//   鼠标进入 → emit preview-entered（浮窗据此取消自动关闭 = pinned）
//   鼠标离开且未聚焦 → emit preview-left（浮窗重启 grace close）
//
// 编辑：textarea 输入防抖 700ms 自动保存（update_todo title）。空标题
// 不保存（后端 validate_title 会拒），hint 行短暂显示错误。
import { invoke, convertFileSrc } from "@tauri-apps/api/core";
import { emit, listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { t, initLocale, onLocaleChange, setLocale, getLocale } from "./i18n";
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

// 底部操作按钮图标（复制 / 垃圾桶删除 —— v0.2.4 起全 App 统一垃圾桶，
// 与 main.ts 卡内按钮同 lucide trash-2 / feather copy 风格，13px 显示）
const COPY_ICON_SVG = `<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>`;
const TRASH_ICON_SVG = `<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M3 6h18"/><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6"/><path d="M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"/><line x1="10" y1="11" x2="10" y2="17"/><line x1="14" y1="11" x2="14" y2="17"/></svg>`;

/// epoch ms → 短日期（"2026/8/5" / "8/5/2026"，随 locale）
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

let currentTodoId: string | null = null;
let currentTodo: Todo | null = null;
let attachmentsDir: string | null = null;
let saveTimer: ReturnType<typeof setTimeout> | null = null;
let closing = false;
const SAVE_DEBOUNCE_MS = 700;

function attachmentUrl(file: string): string | null {
  if (!attachmentsDir) return null;
  const sep = attachmentsDir.endsWith("/") || attachmentsDir.endsWith("\\") ? "" : "/";
  return convertFileSrc(attachmentsDir + sep + file);
}

// ── 渲染 ──

function render(todo: Todo) {
  currentTodo = todo;
  appEl.innerHTML = "";

  const panel = document.createElement("div");
  panel.className = "preview-panel";

  if (todo.attachment) {
    const img = document.createElement("img");
    img.className = "preview-image";
    img.alt = todo.title;
    img.draggable = false;
    const url = attachmentUrl(todo.attachment.file);
    if (url) img.src = url;
    // 附件文件丢失 → 图片区整体摘掉，留文本编辑区
    img.addEventListener("error", () => img.remove());
    panel.appendChild(img);
  }

  const textarea = document.createElement("textarea");
  textarea.className = "preview-text";
  textarea.value = todo.title;
  textarea.spellcheck = false;
  textarea.maxLength = 114514;
  panel.appendChild(textarea);

  // ── 底部 footer（v0.2.4）：左 创建日期 ｜ 中 hint ｜ 右 [完成日期(done)]
  // ＋ 复制按钮 ＋ 垃圾桶删除按钮（二次确认同卡内） ──
  const footer = document.createElement("div");
  footer.className = "preview-footer";

  const created = document.createElement("span");
  created.className = "preview-date";
  created.textContent = `${t("preview.created")} ${formatDate(todo.created_at)}`;
  footer.appendChild(created);

  const hint = document.createElement("div");
  hint.className = "preview-hint";
  hint.textContent = t("preview.hint");
  footer.appendChild(hint);

  const actions = document.createElement("div");
  actions.className = "preview-actions";
  if (todo.status === "done") {
    const doneAt = document.createElement("span");
    doneAt.className = "preview-date";
    doneAt.textContent = `${t("preview.completed")} ${formatDate(todo.updated_at)}`;
    actions.appendChild(doneAt);
  }
  const copyBtn = document.createElement("button");
  copyBtn.className = "preview-action-btn";
  copyBtn.setAttribute("aria-label", t("app.action.copy"));
  copyBtn.innerHTML = COPY_ICON_SVG;
  copyBtn.addEventListener("click", (e) => {
    e.stopPropagation();
    void copyTitle(hint);
  });
  actions.appendChild(copyBtn);
  const delBtn = document.createElement("button");
  delBtn.className = "preview-action-btn preview-delete";
  delBtn.setAttribute("aria-label", t("app.action.delete"));
  delBtn.innerHTML = TRASH_ICON_SVG;
  delBtn.addEventListener("click", (e) => {
    e.stopPropagation();
    // 二次确认（同卡内删除语义）：第一次进确认态（实红），3s 内第二次
    // 点击才真删；超时自动撤销。
    if (delBtn.dataset.confirm === "1") {
      void deleteSelf();
    } else {
      delBtn.dataset.confirm = "1";
      setTimeout(() => delete delBtn.dataset.confirm, 3000);
    }
  });
  actions.appendChild(delBtn);
  footer.appendChild(actions);
  panel.appendChild(footer);

  appEl.appendChild(panel);

  // 输入 → 防抖自动保存。外部 todos-changed 回填时（见 listener）
  // 只在 textarea 未聚焦时覆盖，不打断正在输入的内容。
  textarea.addEventListener("input", () => {
    if (saveTimer) clearTimeout(saveTimer);
    saveTimer = setTimeout(() => {
      saveTimer = null;
      void saveTitle(textarea.value, hint);
    }, SAVE_DEBOUNCE_MS);
  });

  // 拖窗：panel 空白区 / 图片上 mousedown → startDragging。
  // textarea 放行（选中文本 / 聚焦编辑），hint 是文字也放行（无妨，可拖）。
  panel.addEventListener("mousedown", (e) => {
    if (e.button !== 0) return;
    const target = e.target as HTMLElement;
    if (target.closest(".preview-text")) return;
    e.preventDefault();
    win.startDragging().catch((err) =>
      console.debug("[preview] startDragging failed", err),
    );
  });
}

function renderMissing() {
  appEl.innerHTML = "";
  const panel = document.createElement("div");
  panel.className = "preview-panel";
  const msg = document.createElement("div");
  msg.className = "preview-missing";
  msg.textContent = t("preview.missing");
  panel.appendChild(msg);
  appEl.appendChild(panel);
}

// ── 保存 ──

async function saveTitle(value: string, hint?: HTMLElement | null) {
  const id = currentTodoId;
  if (!id) return;
  const trimmed = value.trim();
  // 空标题 / 未改动 → 不保存（后端 validate_title 拒空串，改了也白改）
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
        h.textContent = t("preview.hint");
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
    h.textContent = t("preview.hint");
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

/// 删除当前 todo：后端 emit todos-changed → 当前 id 没了 → closeSelf
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

// ── 关闭 ──

function closeSelf() {
  if (closing) return;
  closing = true;
  flushPendingSave();
  // 先广播再关窗 —— 浮窗状态机（previewTodoId/previewPinnedId）靠它复位。
  // 后端主动 close 的路径（浮窗 grace close / hide）走 beforeunload 补发。
  emit("usticky://preview-closed", {}).catch(() => {});
  win.close().catch((e) => console.debug("[preview] close failed", e));
}

// ── 加载指定 todo ──

async function loadTodo(id: string) {
  try {
    const snap = await invoke<TodoSnapshot>("get_todos");
    const todo = snap.todos.find((x) => x.id === id);
    if (!todo) {
      renderMissing();
      // todo 被删 → 窗口没有存在意义
      closeSelf();
      return;
    }
    currentTodoId = id;
    render(todo);
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
    if (hint) hint.textContent = t("preview.hint");
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
  } else {
    renderMissing();
  }

  // 后端复用窗口：emit 换 todo（hover 在卡间移动时复用同一预览窗）
  let unlistenSetTodo: UnlistenFn | null = null;
  listen<{ id: string }>("usticky://preview-todo", (e) => {
    if (e.payload.id && e.payload.id !== currentTodoId) {
      flushPendingSave();
      void loadTodo(e.payload.id);
    }
  })
    .then((fn) => (unlistenSetTodo = fn))
    .catch((e) => console.error("[preview] listen preview-todo failed", e));

  // 外部数据变化：当前 todo 被删 → 关窗；标题被别处改 → 未聚焦时回填。
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

  // Esc 关闭（QuickLook 语义）。textarea 里按 Esc 也关 —— 编辑有自动保存，
  // 不需要 Esc = 取消编辑的二级语义。
  window.addEventListener("keydown", (e) => {
    if (e.key === "Escape") {
      e.preventDefault();
      closeSelf();
    }
  });

  // 窗口 blur 关闭：用户聚焦过预览窗（点击 / pinned 打开）后点别处 → 收。
  // focused(false) 创建的面板不会收到这个事件（状态没变化过），不受误伤。
  // focused(true) → emit preview-focused：浮窗据此 pin（编辑保护，hover
  // 不换内容）。焦点语义可靠 —— blur 必触发 closeSelf → preview-closed
  // 释放，不存在锁死路径（v0.2.3 起替代 mouseenter 误 pin 的旧路径）。
  let unlistenFocus: UnlistenFn | null = null;
  win
    .onFocusChanged(({ payload: focused }) => {
      if (!focused) {
        closeSelf();
      } else if (currentTodoId) {
        emit("usticky://preview-focused", { id: currentTodoId }).catch(() => {});
      }
    })
    .then((fn) => (unlistenFocus = fn))
    .catch((e) => console.error("[preview] onFocusChanged failed", e));

  // 鼠标进出 → 浮窗 pinned 状态机
  document.body.addEventListener("mouseenter", () => {
    if (!currentTodoId) return;
    emit("usticky://preview-entered", { id: currentTodoId }).catch(() => {});
  });
  document.body.addEventListener("mouseleave", () => {
    if (!currentTodoId) return;
    // 聚焦中的预览窗（编辑态）离开鼠标不解除 pinned —— blur 会负责关。
    win
      .isFocused()
      .then((focused) => {
        if (!focused && currentTodoId) {
          emit("usticky://preview-left", { id: currentTodoId }).catch(() => {});
        }
      })
      .catch(() => {});
  });

  // 后端主动 close（浮窗 grace close / 浮窗 hide 兜底）：补发 preview-closed
  // 让浮窗状态机复位（浮窗自己发起的路径已自清，这里是幂等双保险）。
  window.addEventListener("beforeunload", () => {
    flushPendingSave();
    emit("usticky://preview-closed", {}).catch(() => {});
    unlistenLocale?.();
    unlistenSetTodo?.();
    unlistenTodos?.();
    unlistenFocus?.();
  });

  // prewarm 竞态防线：prewarm 隐藏创建的 webview 可能还没加载完，后端
  // open_preview_window reuse 路径 emit 的 preview-todo 会丢 → 后端在
  // emit 前先存 pending id，这里 listeners 就位后主动取一次。
  try {
    const pendingId = await invoke<string | null>("take_pending_preview_todo");
    if (pendingId && pendingId !== currentTodoId) {
      await loadTodo(pendingId);
    }
  } catch (e) {
    console.debug("[preview] take_pending_preview_todo failed", e);
  }
}

init();
