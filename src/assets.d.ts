// Vite 默认支持静态资源 import，但 tsc 不认 *.png/svg。
// Usticky v0.1 暂不需要 logo 资源，但留着文件方便后续加
// （Musage 当时踩了 CSP + Vite assetsInlineLimit 兼容性坑，详见 AGENTS.md 第 3 节）
declare module "*.png" {
  const src: string;
  export default src;
}
declare module "*.svg" {
  const src: string;
  export default src;
}
declare module "*.jpg" {
  const src: string;
  export default src;
}
declare module "*.jpeg" {
  const src: string;
  export default src;
}
declare module "*.svg?url" {
  const src: string;
  export default src;
}
declare module "*.png?url" {
  const src: string;
  export default src;
}