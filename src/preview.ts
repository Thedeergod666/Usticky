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

// ── 内容高度自适应（编辑时窗高跟着行数走） ──
//
// **绝对式**算高，不是 delta 式（`innerHeight + (H - T)`）：
//   纯文本卡：W = H + PV_CHROME_TEXT_H
//   图片卡  ：W = I_nat + H + PV_CHROME_IMAGE_H
// delta 式对图片卡是错的 —— .preview-image 是 flex:0 1 auto + max-height:100%，
// 它的实测高本身就是窗高的函数（窗一长图就长回去），delta 每次按键都被
// 图片吃掉一截 → 逐键阶梯式长高。绝对式不依赖上一帧布局，天然收敛。
//
// 常量与 Rust preview_logical_size / preview.css 同源，改一处必须全改：
//   app padding 6×2 = 12 | panel border 1×2 = 2 | panel padding 14×2 = 28
//   footer 22 | gap 10（纯文本一个）/ 20（有图两个）
//   纯文本 12+2+28+22+10 = 74；有图 12+2+28+22+20 = 84
const PV_CHROME_TEXT_H = 74;
const PV_CHROME_IMAGE_H = 84;
/// 与 preview.css .preview-text 的 min-height 对齐 —— 目标低于它时 flex
/// 压不下去，窗口反而会被 body overflow:hidden 把 footer 裁掉。
const PV_TEXT_MIN_H = 64;
/// 窗高下限 = 64 + 74。Rust 那边同值（preview_logical_size 的 130 已改 138）。
const PV_MIN_WINDOW_H = 138;
/// 窗高上限，与 preview_logical_size 的 clamp 上界一致。
const PV_MAX_WINDOW_H = 720;
/// resize 尾沿防抖。**不是** rAF —— rAF 只是帧边界不是节流，连续打字
/// 每帧照样发一次 IPC，窗口逐行抖（AGENTS.md #18「输入中禁止 autoResize」
/// 的预览窗等价物）。160ms：一次打字停顿只 resize 一次。
const RESIZE_DEBOUNCE_MS = 160;
/// 小于此差值不发 IPC。3px 吃掉 border-box/scrollHeight 口径残差 +
/// Retina 物理像素 round-trip 的亚像素抖动；一行文字约 19.5px，不会漏。
const RESIZE_MIN_DELTA_PX = 3;

let measurerEl: HTMLDivElement | null = null;
let resizeTimer: ReturnType<typeof setTimeout> | null = null;
/// IME 组合中（中文拼音 / 日文假名）：WebKit 在候选未上屏时就派发 input，
/// 此时 textarea.value 是临时串（"nihao"），照它 resize 会在组合期间抖，
/// 且上屏后（"你好"）没有新的 input 事件来收窄 → 窗口停在错误高度。
/// 组合期间完全跳过，compositionend 补一次。
let imeComposing = false;
/// 换 todo / 重载后作废在途的 debounce 回调 —— prewarm show→loadTodo
/// 之间的排队回调会拿旧内容量出旧高度，正好落在"show 后再 resize"的
/// 闪烁窗口里。
let fitGeneration = 0;
/// 上次请求的目标高 / 后端实际生效高。两者不等 = 被 work_area 或
/// 138-720 clamp 了；此时同一目标高不再重复发 IPC（否则贴屏幕底编辑
/// 时每次输入都空转一次 IPC）。
let lastDesiredH: number | null = null;
let lastAchievedH: number | null = null;

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
    // IME 组合中不 resize（临时串会让窗口在组合期间抖），compositionend 补。
    if (!imeComposing) scheduleFit();
  });
  textarea.addEventListener("compositionstart", () => {
    imeComposing = true;
  });
  textarea.addEventListener("compositionend", () => {
    imeComposing = false;
    scheduleFit();
  });
  // WKWebView 已知行为：从外部拖文本进 textarea **不**派发 input，
  // 只在 blur 时补 change。两个都挂上兜底。drop 时 value 还没更新，
  // 延到下一个宏任务再量。
  textarea.addEventListener("drop", () => {
    setTimeout(() => scheduleFit(), 0);
  });
  textarea.addEventListener("change", () => scheduleFit());
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
        // 图没了 -> 布局从"有图 84"退回"纯文本 74"，重量一次。
        scheduleFit();
      });
      // 图片解码完 naturalW/H 才可用，也才知道它占多高 -> 补一次 fit。
      img.addEventListener("load", () => scheduleFit());
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

// ── 内容高度自适应 ──

/// 量出「装下这段文字，textarea 需要多高（border-box）」。
///
/// 为什么不用 textarea.scrollHeight：textarea 的 scrollHeight 不会小于
/// clientHeight，删行时它一直报着当前高 → 只能长不能缩。
/// 为什么 +2：scrollHeight 含 padding **不含 border**，而我们要的是
/// border-box 高（.preview-text 是 border-box + 1px transparent border）。
/// 少这 2px 每次都欠一点，稳态下最后一行永远被切掉 2px。
/// widthPx 传 textarea 的 **border-box 宽**（getBoundingClientRect().width）
/// 而不是 clientWidth：Win/Linux 经典滚动条占宽时 clientWidth 是"有滚动条
/// 的窄宽"，按它换行会多算行；rect.width 对应的是 resize 完成后（无滚动条）
/// 的目标态，一次到位不来回振荡。
function measureTextH(text: string, widthPx: number): number {
  if (!measurerEl) {
    measurerEl = document.createElement("div");
    measurerEl.className = "preview-measurer";
    document.body.appendChild(measurerEl);
  }
  measurerEl.style.width = `${widthPx}px`;
  // 末尾换行补零宽空格：div 不会为结尾的 "\n" 排出空行盒，textarea 会
  // （光标那一行）。不补的话「按回车换到新行」量不出变化，窗口不动，
  // 等用户敲下一个字才突然跳一整行。
  measurerEl.textContent = text.endsWith("\n") ? `${text}\u200b` : text;
  return measurerEl.scrollHeight + 2;
}

/// 算目标窗高并发 IPC。绝对式，见文件上方常量注释。
async function fitWindowToText() {
  if (closing || !currentTodoId) return;
  // prewarm 隐藏窗 / 已被摘掉的 textarea：量不准也没意义。
  if (document.visibilityState === "hidden") return;
  const textarea = textareaEl;
  if (!textarea || !textarea.isConnected) return;

  const rect = textarea.getBoundingClientRect();
  if (rect.width <= 0) return;

  let need = measureTextH(textarea.value, rect.width);
  // textarea 压不到 64 以下（CSS min-height），目标高必须认这个下限，
  // 否则每次输入都算出一个"比布局能给的更矮"的目标 → 每次都发一遍
  // 缩窗 IPC，窗口一路被走矮到 clamp，footer 被裁掉。
  if (need < PV_TEXT_MIN_H) need = PV_TEXT_MIN_H;

  let desired: number;
  const img = imageEl;
  if (img && img.isConnected && img.naturalWidth > 0 && img.naturalHeight > 0) {
    // 图片的**自然**布局高：按内容宽等比缩放 naturalW/H。
    // **不**读 img.getBoundingClientRect().height —— 图片 max-height:100%
    // 是相对 panel 内容盒（= 窗高的函数），拿实测高进公式会自激。
    const contentW = rect.width;
    const natH =
      img.naturalWidth <= contentW
        ? img.naturalHeight
        : (img.naturalHeight * contentW) / img.naturalWidth;
    desired = Math.round(natH + need + PV_CHROME_IMAGE_H);
  } else if (img && img.isConnected) {
    // 图片还没解码出 naturalSize：等 load 事件再来一次，别用半成品量。
    return;
  } else {
    desired = Math.round(need + PV_CHROME_TEXT_H);
  }
  desired = Math.min(Math.max(desired, PV_MIN_WINDOW_H), PV_MAX_WINDOW_H);

  // 已经就是这个高 -> 不发。
  if (Math.abs(desired - window.innerHeight) < RESIZE_MIN_DELTA_PX) return;
  // 上次就是这个目标、后端 clamp 到了 lastAchievedH、窗口至今没被用户
  // 手动改过 -> 再发也是原地踏步，跳过（贴屏幕底编辑时的 IPC 空转）。
  if (
    lastDesiredH !== null &&
    lastAchievedH !== null &&
    Math.abs(desired - lastDesiredH) < RESIZE_MIN_DELTA_PX &&
    Math.abs(window.innerHeight - lastAchievedH) < RESIZE_MIN_DELTA_PX
  ) {
    return;
  }

  lastDesiredH = desired;
  try {
    // 后端返回**实际生效**的逻辑高（可能被 work_area / 138-720 clamp）。
    lastAchievedH = await invoke<number>("resize_preview_window", { height: desired });
  } catch (e) {
    // 关窗竞态下 set_size 会失败，无需上报。
    lastAchievedH = null;
    console.debug("[preview] resize_preview_window failed", e);
  }
}

/// 尾沿防抖排一次 fit。generation 不匹配（期间换了 todo）就丢弃。
function scheduleFit() {
  if (closing) return;
  const gen = fitGeneration;
  if (resizeTimer) clearTimeout(resizeTimer);
  resizeTimer = setTimeout(() => {
    resizeTimer = null;
    if (gen !== fitGeneration) return;
    void fitWindowToText();
  }, RESIZE_DEBOUNCE_MS);
}

/// 换 todo / 关窗：作废在途 fit 并清掉 clamp 记忆。
function cancelFit() {
  fitGeneration += 1;
  if (resizeTimer) {
    clearTimeout(resizeTimer);
    resizeTimer = null;
  }
  lastDesiredH = null;
  lastAchievedH = null;
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
  cancelFit();
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

/// 加载指定 todo。`todo` 传入了（preview-todo 事件已带完整对象，P1 fix）就
/// 跳过 get_todos 全列表往返直接 render；缺省时（初始 URL 加载 /
/// take_pending_preview_todo 只有 id）才走 get_todos。
async function loadTodo(id: string, todo?: Todo) {
  perfMark("preview-load-start");
  // 换 todo：作废在途 fit。hover 预览的新尺寸由 Rust open_preview_window
  // 的 reuse 路径一次给到位，这里**不**主动 fit（show 后再 resize 是闪的
  // 根源之一 —— commands/mod.rs preview_logical_size 的 doc comment）。
  cancelFit();
  try {
    let resolved = todo ?? null;
    if (!resolved) {
      const snap = await invoke<TodoSnapshot>("get_todos");
      perfMark("preview-get-todos-end");
      perfMeasureEnd("preview-get-todos", "preview-load-start", "preview-get-todos-end");
      resolved = snap.todos.find((x) => x.id === id) ?? null;
    }
    if (!resolved) {
      renderMissing();
      // todo 被删 -> 窗口没有存在意义
      closeSelf();
      return;
    }
    currentTodoId = id;
    perfMark("preview-render-start");
    render(resolved);
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
    // 中英切换会换字体族（PingFang SC ↔ SF Pro Text），同样的文字换行不同。
    scheduleFit();
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
    // **仅固定窗**开窗后主动量一次。hover 预览的尺寸由 Rust 预测量一次
    // 开到位，不能再 resize；固定窗（pin_preview）在没有 hover 预览可
    // 抄尺寸时会退到硬编码 460×340（commands/mod.rs pin_preview 兜底），
    // 长文会被截在窗内滚动 —— 这一次校正就是给这条路径的。
    if (isPinned) scheduleFit();
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
  listen<{ id: string; todo?: Todo }>("usticky://preview-todo", (e) => {
    if (isPinned) return;
    if (e.payload.id && e.payload.id !== currentTodoId) {
      flushPendingSave();
      // P1 fix：事件已带完整 todo，直接传进去省 get_todos 往返
      void loadTodo(e.payload.id, e.payload.todo);
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
    // **KEEP**：这个 :focus 守卫是 save → todos-changed → 回填 → resize
    // 回声环的断点。聚焦中不回填 = 不产生新的内容变化 = 不再排 fit。
    // 谁要把它放松成"实时同步"，必须同时给 fit 加内容指纹去重。
    if (textarea && !textarea.matches(":focus") && textarea.value !== todo.title) {
      textarea.value = todo.title;
      scheduleFit();
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
    cancelFit();
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
