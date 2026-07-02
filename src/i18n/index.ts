// i18n —— 自写 helper，Musage 双 locale 架构的精简版。
//
// 关键约定（沿用 Musage，见 AGENTS.md）：
//   - dict key 用 . 分层（`button.save` 不是 `Save`），跟后端 rust-i18n 对齐
//   - 占位符命名 `{name}` —— 跟 rust_i18n::t!() 严格一致
//   - JSON 文件里中文必须用全角引号 `『』` 或 `\"` 转义，否则 position 19xxx 报错
//   - locale 切换：先 load dict，再 setLocale()，最后通知所有订阅者

import enDict from "./en.json";
import zhCNDict from "./zh-CN.json";

type Dict = Record<string, string>;

const DICTS: Record<string, Dict> = {
  en: enDict as Dict,
  "zh-CN": zhCNDict as Dict,
};

let currentLocale: string = "zh-CN";  // 默认 zh-CN，跟 Musage 一致
let currentDict: Dict = DICTS[currentLocale];

// 订阅者：locale 变化时回调（用来重建 UI 文本）
type LocaleChangeListener = (newLocale: string) => void;
const listeners: LocaleChangeListener[] = [];

export function onLocaleChange(fn: LocaleChangeListener): () => void {
  listeners.push(fn);
  return () => {
    const idx = listeners.indexOf(fn);
    if (idx >= 0) listeners.splice(idx, 1);
  };
}

export function getLocale(): string {
  return currentLocale;
}

/// 启动时从后端拉当前 locale + dict。**必须在任何 t() 调用前**完成。
export async function initLocale(): Promise<void> {
  try {
    // 动态 import 避免 vite 静态分析把 @tauri-apps/api 拖进 SSR 等场景
    const { invoke } = await import("@tauri-apps/api/core");
    const locale = await invoke<string>("get_app_locale");
    if (DICTS[locale]) {
      setLocale(locale);
    }
  } catch {
    // 离线 / 非 Tauri 环境（纯 vite dev / storybook）走默认 locale
    console.debug("[i18n] initLocale 走默认 zh-CN");
  }
  // 同步 html lang 属性
  document.documentElement.lang = currentLocale;
}

export function setLocale(locale: string): void {
  if (!DICTS[locale]) {
    console.warn(`[i18n] 未知 locale: ${locale}，保持 ${currentLocale}`);
    return;
  }
  currentLocale = locale;
  currentDict = DICTS[locale];
  document.documentElement.lang = locale;
  for (const fn of listeners) fn(locale);
}

/// 翻译函数。支持 `{name}` 占位符替换。
/// 找不到 key 时返 key 本身（开发期一眼能看到漏翻译的 key）。
export function t(key: string, params?: Record<string, string | number>): string {
  let str = currentDict[key];
  if (str === undefined) {
    // 走 fallback en
    str = DICTS.en[key];
  }
  if (str === undefined) {
    console.warn(`[i18n] missing key: ${key}`);
    return key;
  }
  if (params) {
    for (const [k, v] of Object.entries(params)) {
      str = str.replace(new RegExp(`\\{${k}\\}`, "g"), String(v));
    }
  }
  return str;
}