// Usticky 性能埋点工具（Stage 4 — PERF_AUDIT.md 测量脚本配套）
//
// 设计：webview 侧用 performance.mark/measure 打点，**不**直接读 Tauri 的
// 内部计时；需要落盘到外部文件时调 dump()，由后端 dump_perf 命令原子写入
// （USTICKY_PERF_OUT 环境变量指定路径）。
//
// 业务代码不应该 import 这个模块做高频埋点（mark/measure 自身有开销）；
// 只在三个 S 级候选的关键路径用：init() 启动阶段、preview loadTodo()、
// hover listener 入口。
//
// 所有 mark/measure 调用都用 try/catch 包裹：mark(name) 重复同名会抛
// InvalidMarkName 等，吞掉不让性能埋点把业务路径打崩。
import { invoke } from "@tauri-apps/api/core";

/// 打点。失败静默（性能埋点不该 crash 业务）。
export function mark(name: string): void {
  try { performance.mark(name); } catch { /* 重复 mark 等 */ }
}

/// 结束一个 measure 区间。startName 是已存在的 mark，endName 缺省 = now。
/// startName 不存在时 measure API 抛错，吞掉。
export function endMeasure(label: string, startName: string, endName?: string): void {
  try {
    if (endName) performance.measure(label, startName, endName);
    else performance.measure(label, startName);
  } catch { /* 起点 mark 不存在 */ }
}

/// 把当前所有 mark/measure + 任意 extra 上下文写入指定文件（通过后端
/// dump_perf 命令；后端负责原子写 + 创建父目录）。
///
/// extra 典型用法：dump(perfOut, { ready: Date.now(), iter: idx })
/// 让外部脚本能把 perf mark 的 relative 时间和脚本侧的 absolute time
/// 对齐。
export async function dump(outPath: string, extra: Record<string, unknown> = {}): Promise<void> {
  if (!outPath) return;
  const marks = performance.getEntriesByType("mark").map((e) => ({ name: e.name, t: e.startTime }));
  const measures = performance.getEntriesByType("measure").map((e) => ({
    name: e.name,
    start: e.startTime,
    duration: e.duration,
  }));
  const data = { marks, measures, extra };
  try {
    await invoke("dump_perf", { path: outPath, data });
  } catch (e) {
    console.error("[perf] dump failed", e);
  }
}

/// 清空所有 mark/measure（多次 dump 时避免旧数据混入）。
export function clear(): void {
  try {
    performance.clearMarks();
    performance.clearMeasures();
  } catch { /* */ }
}
