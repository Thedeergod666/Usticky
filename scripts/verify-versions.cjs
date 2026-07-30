// scripts/verify-versions.cjs
// 验证 package.json / Cargo.toml / tauri.conf.json 三处 version 一致
// 用于 CI 的 "Verify versions consistent" step。
// 跨平台：纯 Node，无 shell 依赖。

const fs = require("fs");

const pkgVer = JSON.parse(fs.readFileSync("package.json", "utf8")).version;
const cargoText = fs.readFileSync("src-tauri/Cargo.toml", "utf8");
const tauriVer = JSON.parse(
  fs.readFileSync("src-tauri/tauri.conf.json", "utf8"),
).version;

// 匹配 Cargo.toml 第一行 `version = "x.y.z"`（多行模式；避开 dependency 中的 version）
const cargoMatch = cargoText.match(/^version\s*=\s*"([^"]+)"\s*$/m);
const cargoVer = cargoMatch ? cargoMatch[1] : "(not found)";

console.log("package.json  =", pkgVer);
console.log("Cargo.toml    =", cargoVer);
console.log("tauri.conf    =", tauriVer);

if (cargoVer !== pkgVer || tauriVer !== pkgVer) {
  console.error(
    "::error::version mismatch: package.json=" +
      pkgVer +
      " Cargo.toml=" +
      cargoVer +
      " tauri.conf=" +
      tauriVer,
  );
  process.exit(1);
}

console.log("✓ package.json = Cargo.toml = tauri.conf.json = " + pkgVer);
