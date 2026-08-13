// scripts/perf-baseline.mjs — Usticky 冷启动 + 预览换卡性能基线测量
//
// 对应 PERF_AUDIT.md Stage 4：跑 N 次冷启，采集 webview 侧 performance
// mark/measure 落盘文件（后端 dump_perf 命令写），统计 p50/p95 写报告。
//
// 自动化范围：冷启动 -> 首次 render（S1 fix 后应该降 30%+）。
// 手动范围：preview 换卡（S2 改后应当明显）、hover 移动（A1/S3）、
// 静止 10s 事件计数（S3）—— 这些需 UI 驱动，不在 baseline 跑。
//
// 用法：
//   pnpm perf:baseline           # 20 次迭代，release 二进制
//   pnpm perf:baseline 5         # 5 次迭代
//   pnpm perf:baseline 20 debug  # 用 debug 编译产物
// 前提：target/release/usticky 或 target/debug/usticky 已存在（pnpm tauri:build
// 或 pnpm tauri:build --debug 先跑）。脚本直接 spawn 二进制，不调 tauri dev
// —— 避免 dev 模式 HMR / 调试包拖慢冷启。

import { spawn, execSync } from "node:child_process";
import {
  existsSync,
  readFileSync,
  writeFileSync,
  mkdirSync,
  unlinkSync,
} from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";

const ITER = parseInt(process.argv[2] ?? "20", 10);
const PROFILE = process.argv[3] ?? "release";
const TMP_DIR = join(tmpdir(), "usticky-perf");
mkdirSync(TMP_DIR, { recursive: true });

const BINARY =
  PROFILE === "debug"
    ? "src-tauri/target/debug/usticky"
    : "src-tauri/target/release/usticky";

if (!existsSync(BINARY)) {
  console.error(
    `[perf-baseline] 找不到二进制：${BINARY}\n` +
      `  请先跑：pnpm tauri:build${PROFILE === "debug" ? " --debug" : ""}`,
  );
  process.exit(1);
}

/// **P3-30 fix（2026-08-13 全量审查）**：检测已运行的 Usticky 实例。
///
/// tauri-plugin-single-instance（v0.1.5 集成）会让第二次启动的进程把请求
/// 转发给已运行实例后立即退出 —— 用户开着 Usticky（常驻应用，概率很高）
/// 再跑 baseline：每轮 spawn 的进程都被转发 + 退出，永远写不出
/// USTICKY_PERF_OUT → 每轮 20s 超时标记 FAILED、20 轮 400s+ 全灭且无
/// 任何报错指引。提前用 pgrep / tasklist 探测，命中直接给明确指引。
function detectRunningUsticky() {
  try {
    if (process.platform === "win32") {
      const out = execSync('tasklist /FI "IMAGENAME eq usticky.exe" /FO CSV /NH', {
        encoding: "utf8",
        stdio: ["ignore", "pipe", "ignore"],
      });
      return /usticky\.exe/i.test(out);
    }
    const out = execSync("pgrep -x usticky", {
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
    });
    return out.trim().length > 0;
  } catch {
    // pgrep 无匹配 = exit 1 → 没在跑；execSync 自身报错 = 工具不可用 → 不阻塞
    return false;
  }
}

function percentile(arr, p) {
  if (arr.length === 0) return 0;
  const sorted = [...arr].sort((a, b) => a - b);
  const idx = Math.min(sorted.length - 1, Math.floor((p / 100) * sorted.length));
  return sorted[idx];
}

function stats(arr) {
  if (arr.length === 0) return { n: 0, p50: 0, p95: 0, mean: 0 };
  const mean = arr.reduce((s, x) => s + x, 0) / arr.length;
  return { n: arr.length, p50: percentile(arr, 50), p95: percentile(arr, 95), mean };
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function runOne(idx) {
  const outPath = join(TMP_DIR, `iter-${String(idx).padStart(3, "0")}.json`);
  if (existsSync(outPath)) {
    try { unlinkSync(outPath); } catch {}
  }

  const env = { ...process.env, USTICKY_PERF_OUT: outPath };
  const proc = spawn(BINARY, [], { env, stdio: "ignore", detached: false });

  const deadline = Date.now() + 20_000;
  let data = null;
  while (Date.now() < deadline) {
    if (existsSync(outPath)) {
      await sleep(300);
      try {
        const raw = readFileSync(outPath, "utf8");
        data = JSON.parse(raw);
        break;
      } catch {
        await sleep(100);
      }
    } else {
      await sleep(100);
    }
  }

  try { proc.kill("SIGTERM"); } catch {}
  await sleep(300);
  if (proc.exitCode === null) {
    try { proc.kill("SIGKILL"); } catch {}
  }

  return data;
}

function findMark(file, name) {
  const m = file.marks.find((x) => x.name === name);
  return m ? m.t : null;
}

function findMeasure(file, name) {
  const m = file.measures.find((x) => x.name === name);
  return m ? m.duration : null;
}

async function main() {
  // P3-30：单实例冲突前置检查 —— 明确报错而不是 400s 静默全灭。
  if (detectRunningUsticky()) {
    console.error(
      "[perf-baseline] 检测到 Usticky 已在运行（tauri-plugin-single-instance 会把新进程转发给已运行实例后退出，baseline 永远拿不到数据）。\n" +
        "  请先退出 Usticky（tray 菜单 Quit / Cmd+Q）再跑本脚本。",
    );
    process.exit(1);
  }

  console.log(`[perf-baseline] ${ITER} iterations against ${BINARY}`);
  console.log(`[perf-baseline] tmp dir: ${TMP_DIR}\n`);

  const results = [];
  for (let i = 0; i < ITER; i++) {
    const t0 = Date.now();
    const r = await runOne(i);
    const elapsed = Date.now() - t0;
    if (r) {
      results.push(r);
      process.stdout.write(`\r[${i + 1}/${ITER}] ok in ${elapsed}ms`);
    } else {
      process.stdout.write(`\r[${i + 1}/${ITER}] FAILED (${elapsed}ms)`);
    }
  }
  process.stdout.write("\n\n");

  if (results.length === 0) {
    console.error("[perf-baseline] 没有成功完成的迭代，退出。");
    process.exit(1);
  }

  const rows = [];

  const bootLocale = results
    .map((f) => {
      const a = findMark(f, "boot-start");
      const b = findMark(f, "boot-locale");
      return a !== null && b !== null ? b - a : null;
    })
    .filter((x) => x !== null);
  rows.push({
    label: "boot-start -> boot-locale (initLocale)",
    src: "delta",
    refs: "initLocale",
    arr: bootLocale,
    unit: "ms",
  });

  const parallelPhase = results
    .map((f) => findMeasure(f, "invoke-parallel"))
    .filter((x) => x !== null);
  rows.push({
    label: "invoke-parallel (Promise.all of 3)",
    src: "measure",
    refs: "S1",
    arr: parallelPhase,
    unit: "ms",
  });

  const todosPhase = results
    .map((f) => findMeasure(f, "invoke-todos"))
    .filter((x) => x !== null);
  rows.push({
    label: "invoke-todos (get_todos)",
    src: "measure",
    refs: "S1",
    arr: todosPhase,
    unit: "ms",
  });

  const bootTotal = results
    .map((f) => findMeasure(f, "boot-total"))
    .filter((x) => x !== null);
  rows.push({
    label: "boot-total (start -> first render)",
    src: "measure",
    refs: "S1",
    arr: bootTotal,
    unit: "ms",
  });

  console.log("=== Cold start to first render ===");
  console.log(
    "label".padEnd(50) + "  n  " + "p50".padStart(8) + "  " + "p95".padStart(8) + "  " + "mean".padStart(8),
  );
  console.log("-".repeat(86));
  for (const r of rows) {
    if (r.arr.length === 0) continue;
    const s = stats(r.arr);
    const lbl = `${r.label} [${r.refs}]`;
    console.log(
      lbl.padEnd(50) + "  " +
        String(s.n).padStart(2) + "  " +
        s.p50.toFixed(1).padStart(6) + "ms" + "  " +
        s.p95.toFixed(1).padStart(6) + "ms" + "  " +
        s.mean.toFixed(1).padStart(6) + "ms",
    );
  }

  const reportDir = join("dist", "perf-reports");
  mkdirSync(reportDir, { recursive: true });
  const stamp = new Date().toISOString().replace(/[:.]/g, "-");
  const reportPath = join(reportDir, `baseline-${PROFILE}-${stamp}.json`);
  const report = {
    binary: BINARY,
    iter: results.length,
    timestamp: new Date().toISOString(),
    rows: rows.map((r) => ({ ...r, stats: stats(r.arr) })),
    rawMarks: results.map((f) => f.marks),
    rawMeasures: results.map((f) => f.measures),
  };
  writeFileSync(reportPath, JSON.stringify(report, null, 2));
  console.log(`\n[perf-baseline] report: ${reportPath}`);
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
