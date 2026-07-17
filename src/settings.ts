// Usticky 设置面板入口
//
// 单页设计（不拆 nav tabs）：
//   1. 浮窗层级（pin mode）— segmented control
//   2. 语言 — en / zh-CN
//   3. 浮窗 — 归位到主屏幕正中央
//   4. 快速唤出快捷键 — 点击按钮 → 录入新组合键
//   5. 关于 — 版本 / 产品名 / GitHub
//
// 复用浮窗的 i18n helper（src/i18n/index.ts），保持单一来源 = 后端 locales。
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getVersion } from "@tauri-apps/api/app";
import { openUrl } from "@tauri-apps/plugin-opener";
import { t, initLocale, onLocaleChange, setLocale, getLocale } from "./i18n";
// settings "关于" 区域要显示应用 logo —— Vite 把 src-tauri/icons/icon.png 的 hash 化 URL 注入，
// 单⼀来源 = scripts/generate_icons.py（重跑后 settings 资源自动同步）。
// 不能用 <img src="/icon.png"> —— 项目没有 public/，frontendDist 里就没这个文件，裂成 [?]
import appIconPng from "../src-tauri/icons/icon.png";
import "./settings.css";

type PinMode = "pin_top" | "pin_bottom" | "normal";
type Locale = "en" | "zh-CN";

const root = document.getElementById("settings-app")!;

// ── 快捷键录入状态机 ──
//   idle: 显示当前快捷键（按钮 label = display form）
//   recording: 监听 keydown，下次有效按键组合 → 调 set_quick_add_shortcut
//
// 录入规则：
//   - 单纯 modifier 按下（Shift/Ctrl/Cmd/Alt 单独）不退出 recording
//   - 有效 key（含至少一个 modifier，避免单键冲突系统快捷键）→ 录入
//   - Esc 取消，回到 idle
//   - 点击按钮外区域不取消（用户可能要去按某个键）—— 由 Esc 主动退出
let recording = false;
let currentShortcut: string = "Cmd+Shift+Space";  // 启动时被 get_quick_add_shortcut 覆盖

// ── mini flash ──
let flashTimer: ReturnType<typeof setTimeout> | null = null;
function flash(msg: string): void {
  let el = document.querySelector<HTMLElement>(".flash");
  if (!el) {
    el = document.createElement("div");
    el.className = "flash";
    document.body.appendChild(el);
  }
  el.textContent = msg;
  el.classList.add("visible");
  if (flashTimer) clearTimeout(flashTimer);
  flashTimer = setTimeout(() => el?.classList.remove("visible"), 2200);
}

function escapeHtml(s: string): string {
  return s.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]!));
}

/// 把 accelerator 字符串（如 `"Cmd+Shift+Space"`）转成展示形式（macOS: `⌘⇧Space`）。
/// 跟后端 tray.rs 的 format_shortcut_for_display 同款逻辑，前端单独实现一份
/// （浮窗 input hint 也要用，避免每个 webview 都 invoke 一次后端）。
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

/// 把 keydown 事件转成 accelerator 字符串（如 `"Cmd+Shift+Space"`）。
/// 单独 modifier 按下返 null（等用户接着按真键）。
/// 不带任何 modifier 也返 null（避免单键冲突系统快捷键）。
function keyEventToAccelerator(e: KeyboardEvent): string | null {
  // 单 modifier 按键不退出录入
  if (e.key === "Meta" || e.key === "Control" || e.key === "Shift" || e.key === "Alt") {
    return null;
  }
  // 必须有至少一个 modifier —— 单字母 / 单数字不允许（容易跟系统单键冲突）
  if (!e.metaKey && !e.ctrlKey && !e.shiftKey && !e.altKey) {
    return null;
  }
  const parts: string[] = [];
  if (e.metaKey) parts.push("Cmd");
  if (e.ctrlKey) parts.push("Ctrl");
  if (e.shiftKey) parts.push("Shift");
  if (e.altKey) parts.push("Alt");
  // 规范化 key 名 → global-hotkey 接受的形式
  let key: string;
  if (e.key === " ") key = "Space";
  else if (e.key.length === 1) key = e.key.toUpperCase();
  else if (e.key === "Enter") key = "Enter";
  else if (e.key === "Escape") key = "Escape";
  else if (e.key === "Tab") key = "Tab";
  else if (e.key === "ArrowUp") key = "ArrowUp";
  else if (e.key === "ArrowDown") key = "ArrowDown";
  else if (e.key === "ArrowLeft") key = "ArrowLeft";
  else if (e.key === "ArrowRight") key = "ArrowRight";
  else if (e.key === "Backspace") key = "Backspace";
  else if (e.key === "Delete") key = "Delete";
  else if (e.key === "Home") key = "Home";
  else if (e.key === "End") key = "End";
  else if (e.key === "PageUp") key = "PageUp";
  else if (e.key === "PageDown") key = "PageDown";
  else if (/^F[1-9]$|^F1[0-9]$|^F2[0-4]$/.test(e.key)) key = e.key;
  else if (e.key.length === 1 && /[a-zA-Z0-9]/.test(e.key)) key = e.key.toUpperCase();
  else return null;  // 不识别的键 → 不录入（避免存到 store 后 parse 失败）
  parts.push(key);
  return parts.join("+");
}

// ── 渲染 ──

let currentPinMode: PinMode = "pin_bottom";
let currentLocale: Locale = "zh-CN";
let appVersion = "0.1.0";

function render(): void {
  // 标题
  document.title = t("settings.title");

  root.innerHTML = `
    <div class="settings-title">${escapeHtml(t("settings.title"))}</div>

    <section class="section">
      <div class="section-header">
        <div class="section-title">${escapeHtml(t("settings.section.window"))}</div>
      </div>
      <div class="section-body">
        <div class="row">
          <div class="row-label">${escapeHtml(t("settings.pin.label"))}</div>
          <div class="segmented" data-pin>
            <button data-pin-value="pin_top">${escapeHtml(t("settings.pin.top"))}</button>
            <button data-pin-value="pin_bottom">${escapeHtml(t("settings.pin.bottom"))}</button>
            <button data-pin-value="normal">${escapeHtml(t("settings.pin.normal"))}</button>
          </div>
        </div>
        <div class="row">
          <div>
            <div class="row-label">${escapeHtml(t("settings.reset.label"))}</div>
            <div class="row-hint">${escapeHtml(t("settings.reset.hint"))}</div>
          </div>
          <button class="btn" data-action="reset-window">${escapeHtml(t("settings.reset.button"))}</button>
        </div>
      </div>
    </section>

    <section class="section">
      <div class="section-header">
        <div class="section-title">${escapeHtml(t("settings.section.language"))}</div>
      </div>
      <div class="section-body">
        <div class="row">
          <div class="row-label">${escapeHtml(t("settings.language.label"))}</div>
          <div class="segmented" data-locale>
            <button data-locale-value="en">English</button>
            <button data-locale-value="zh-CN">中文</button>
          </div>
        </div>
      </div>
    </section>

    <section class="section">
      <div class="section-header">
        <div class="section-title">${escapeHtml(t("settings.shortcut.label"))}</div>
      </div>
      <div class="section-body">
        <div class="row">
          <div>
            <div class="row-label">${escapeHtml(t("settings.shortcut.label"))}</div>
            <div class="row-hint" data-shortcut-hint>${escapeHtml(t("settings.shortcut.hint"))}</div>
          </div>
          <button class="btn shortcut-btn" data-action="record-shortcut">${escapeHtml(formatShortcutForDisplay(currentShortcut))}</button>
        </div>
      </div>
    </section>

    <section class="section">
      <div class="section-header">
        <div class="section-title">${escapeHtml(t("settings.section.about"))}</div>
      </div>
      <div class="section-body">
        <div class="about-logo">
          <img src="${appIconPng}" alt="Usticky" />
          <div>
            <div class="about-name">Usticky</div>
            <div class="about-version">v${escapeHtml(appVersion)}</div>
          </div>
        </div>
        <div class="about-meta">
          ${escapeHtml(t("settings.about.tagline"))}<br />
          <a data-action="open-github">GitHub</a>
        </div>
      </div>
    </section>
  `;

  refreshPinSegmented();
  refreshLocaleSegmented();
}

function refreshPinSegmented(): void {
  root.querySelectorAll<HTMLElement>("[data-pin] button").forEach((btn) => {
    btn.classList.toggle("active", btn.dataset.pinValue === currentPinMode);
  });
}

function refreshLocaleSegmented(): void {
  root.querySelectorAll<HTMLElement>("[data-locale] button").forEach((btn) => {
    btn.classList.toggle("active", btn.dataset.localeValue === currentLocale);
  });
}

// ── 事件代理 ──
root.addEventListener("click", async (e) => {
  const target = e.target as HTMLElement;

  // pin mode
  const pinBtn = target.closest<HTMLElement>("[data-pin-value]");
  if (pinBtn) {
    const newMode = pinBtn.dataset.pinValue as PinMode | undefined;
    if (newMode && newMode !== currentPinMode) {
      try {
        await invoke("set_pin_mode", { mode: newMode });
        // 后端 emit usticky://pin-mode-changed，listener 会更新 currentPinMode + UI
      } catch (err) {
        console.error("[usticky] set_pin_mode failed", err);
        flash(t("settings.error.save_failed"));
      }
    }
    return;
  }

  // locale
  const localeBtn = target.closest<HTMLElement>("[data-locale-value]");
  if (localeBtn) {
    const newLocale = localeBtn.dataset.localeValue as Locale | undefined;
    if (newLocale && newLocale !== currentLocale) {
      try {
        await invoke("set_app_locale", { locale: newLocale });
        // 后端 emit usticky://locale-changed，listener 会更新 dict + re-render
      } catch (err) {
        console.error("[usticky] set_app_locale failed", err);
        flash(t("settings.error.save_failed"));
      }
    }
    return;
  }

  // reset window
  if (target.closest("[data-action='reset-window']")) {
    try {
      await invoke("reset_floating_window");
      flash(t("settings.reset.done"));
    } catch (err) {
      console.error("[usticky] reset_floating_window failed", err);
      flash(t("settings.reset.failed"));
    }
    return;
  }

  // shortcut 录入按钮：点击切换 recording 状态
  if (target.closest("[data-action='record-shortcut']")) {
    setRecording(!recording);
    return;
  }

  // open github
  if (target.closest("[data-action='open-github']")) {
    try {
      await openUrl("https://github.com/Thedeergod666/Usticky");
    } catch (err) {
      console.error("[usticky] openUrl failed", err);
    }
    return;
  }
});

// ── 快捷键录入 ──
//
// recording 期间全局监听 keydown（capture 阶段，避免被其他 handler 拦截）：
//   - Esc → 取消，回 idle
//   - 单 modifier（Meta/Control/Shift/Alt）→ 忽略，等用户继续按
//   - 不带 modifier → 忽略（单键会跟系统快捷键冲突）
//   - 有效组合 → 调 set_quick_add_shortcut，成功后退 idle + 刷新 label
function setRecording(on: boolean): void {
  recording = on;
  const btn = root.querySelector<HTMLElement>(".shortcut-btn");
  const hint = root.querySelector<HTMLElement>("[data-shortcut-hint]");
  if (btn) {
    if (on) {
      btn.classList.add("recording");
      btn.textContent = t("settings.shortcut.recording");
    } else {
      btn.classList.remove("recording");
      btn.textContent = formatShortcutForDisplay(currentShortcut);
    }
  }
  if (hint) {
    hint.textContent = on ? t("settings.shortcut.cancel_hint") : t("settings.shortcut.hint");
  }
}

document.addEventListener("keydown", async (e) => {
  if (!recording) return;
  // capture 阶段拦截，避免被其他 handler 处理
  e.preventDefault();
  e.stopPropagation();
  // Esc 取消
  if (e.key === "Escape") {
    setRecording(false);
    return;
  }
  const acc = keyEventToAccelerator(e);
  if (!acc) return;  // 单 modifier 或不带 modifier → 等下次按键
  // 录入 → 调后端
  try {
    await invoke("set_quick_add_shortcut", { accelerator: acc });
    currentShortcut = acc;
    setRecording(false);
  } catch (err) {
    console.error("[usticky] set_quick_add_shortcut failed", err);
    flash(t("settings.shortcut.invalid"));
    // 留在 recording 状态让用户重试
  }
}, true);

// ── 启动 ──
async function init(): Promise<void> {
  await initLocale();
  currentLocale = getLocale() as Locale;

  // 拉 pin mode
  try {
    currentPinMode = await invoke<PinMode>("get_pin_mode");
  } catch (e) {
    console.error("[usticky] get_pin_mode failed", e);
  }

  // 拉 quick-add 快捷键
  try {
    currentShortcut = await invoke<string>("get_quick_add_shortcut");
  } catch (e) {
    console.error("[usticky] get_quick_add_shortcut failed", e);
  }

  // 拉 app 版本
  try {
    appVersion = await getVersion();
  } catch (e) {
    console.debug("[usticky] getVersion failed, using default", e);
  }

  render();

  // locale 切换：re-render（pin mode 按钮 / 标题等 i18n 文案要更新）
  onLocaleChange((newLocale) => {
    currentLocale = newLocale as Locale;
    render();
  });

  // 监听后端 locale-changed（来自 tray 菜单 / 浮窗的切换）
  let unlistenLocaleEvt: UnlistenFn | null = null;
  listen<string>("usticky://locale-changed", async (e) => {
    const newLocale = e.payload;
    if (newLocale === "en" || newLocale === "zh-CN") {
      if (newLocale !== getLocale()) await setLocale(newLocale);
    }
  })
    .then((fn) => (unlistenLocaleEvt = fn))
    .catch((e) => console.error("[usticky] listen locale-changed failed", e));

  // 监听后端 pin-mode-changed（来自浮窗 foot 的切换）
  let unlistenPin: UnlistenFn | null = null;
  listen<PinMode>("usticky://pin-mode-changed", (e) => {
    if (e.payload !== currentPinMode) {
      currentPinMode = e.payload;
      refreshPinSegmented();
    }
  })
    .then((fn) => (unlistenPin = fn))
    .catch((e) => console.error("[usticky] listen pin-mode-changed failed", e));

  // 监听后端 shortcut-changed（来自 tray 子菜单 / 其他 webview 改快捷键）
  let unlistenShortcut: UnlistenFn | null = null;
  listen<string>("usticky://shortcut-changed", (e) => {
    if (e.payload !== currentShortcut) {
      currentShortcut = e.payload;
      // 不在 recording 状态时才刷按钮 label —— 否则会覆盖 "Press keys…"
      if (!recording) {
        const btn = root.querySelector<HTMLElement>(".shortcut-btn");
        if (btn) btn.textContent = formatShortcutForDisplay(currentShortcut);
      }
    }
  })
    .then((fn) => (unlistenShortcut = fn))
    .catch((e) => console.error("[usticky] listen shortcut-changed failed", e));

  window.addEventListener("beforeunload", () => {
    unlistenLocaleEvt?.();
    unlistenPin?.();
    unlistenShortcut?.();
  });
}

init();
