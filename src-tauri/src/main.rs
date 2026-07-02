// Windows / Linux 入口 —— macOS 用 src-tauri/src/main_macos.rs（如果有需要拆分时再加）
//
// 大部分逻辑都在 lib.rs 的 `run()` 函数里 —— Tauri 2 推荐把应用 setup 放 lib，
// 二进制入口只负责调用。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    usticky_lib::run();
}