import { fileURLToPath, URL } from "node:url";
import { defineConfig } from "vite";

// Tauri 推荐的 Vite 配置：固定端口 + 监听。
// 关键点（Musage v0.2 已踩过，详见 ~/Project/Usticky/AGENTS.md）：
//   - port=1421 / strictPort / host=127.0.0.1（tauri.conf.json devUrl 对齐）
//   - assetsInlineLimit: 0（CSP `default-src 'self'` 不放 data: URI，
//     不关的话 <4KB 资源会被 Vite 内联成 data: 然后被 CSP block 裂图）
//   - rollupOptions.input 列出所有 HTML entry（Usticky 只有 index.html 一个，
//     但留着 pattern 方便加 settings.html 时扩）
//   - 关 modulePreload polyfill —— 跨平台 HTML byte-for-byte 一致
const root = fileURLToPath(new URL("./", import.meta.url));

export default defineConfig({
  clearScreen: false,
  server: {
    port: 1421,
    strictPort: true,
    host: "127.0.0.1",
    watch: {
      ignored: ["**/src-tauri/**"],
    },
  },
  envPrefix: ["VITE_", "TAURI_"],
  build: {
    target: "esnext",
    minify: "esbuild",
    sourcemap: false,
    assetsInlineLimit: 0,
    modulePreload: { polyfill: false, resolveDependencies: () => [] },
    rollupOptions: {
      input: {
        main: `${root}index.html`,
      },
      output: {
        entryFileNames: "assets/[name].js",
        chunkFileNames: "assets/[name].js",
        assetFileNames: "assets/[name][extname]",
      },
    },
  },
});