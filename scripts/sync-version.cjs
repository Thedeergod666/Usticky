#!/usr/bin/env node
// 把 package.json 的 version 同步到 tauri.conf.json + Cargo.toml
// Musage 一直用这个脚本 —— 改一处版本号 = 三处同步

const fs = require("fs");
const path = require("path");

const pkg = JSON.parse(fs.readFileSync(path.join(__dirname, "..", "package.json"), "utf8"));
const ver = pkg.version;

const tauriPath = path.join(__dirname, "..", "src-tauri", "tauri.conf.json");
const cargoPath = path.join(__dirname, "..", "src-tauri", "Cargo.toml");

// tauri.conf.json
const tauri = JSON.parse(fs.readFileSync(tauriPath, "utf8"));
tauri.version = ver;
fs.writeFileSync(tauriPath, JSON.stringify(tauri, null, 2) + "\n");

// Cargo.toml — 仅当 version 不一致时改
let cargo = fs.readFileSync(cargoPath, "utf8");
const match = cargo.match(/^version\s*=\s*"([^"]+)"\s*$/m);
if (match && match[1] !== ver) {
  cargo = cargo.replace(/^version\s*=\s*"[^"]+"\s*$/m, `version = "${ver}"`);
  fs.writeFileSync(cargoPath, cargo);
}

console.log(`✓ synced version ${ver} → tauri.conf.json + Cargo.toml`);