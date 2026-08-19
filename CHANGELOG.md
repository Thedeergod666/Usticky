# Changelog

所有值得记录的变更都会写到这里。格式基于 [Keep a Changelog](https://keepachangelog.com/)。

## [Unreleased]

## [0.2.5] - 2026-08-19

v0.2.0 之后 46 个 commit 的聚合 release。三块主线：

1. **剪贴板粘贴 + QuickLook 预览窗** —— 图片附件内联展示 + 预览弹出窗
2. **删除撤销 + 附件延迟删除** —— 8s 内可撤销，硬删前先延迟删附件
3. **玻璃 perf + 预览窗即时跟手** —— 背景搬到 `::before` + hover 80ms 即出

### Added

- **剪贴板粘贴按钮**（输入行最右侧，`.todo-paste-btn`）：文本 → 整段
  作 todo；图片 → 落盘 `<data_dir>/attachments/<uuid>.<ext>` + 带
  attachment 的 todo（GIF 保留动画，体积上限 25MB）。macOS 走
  NSPasteboard 原始字节（`public.gif` / `public.file-url` /
  `public.tiff` 三路径），Win + Linux 走插件 RGBA → PNG（GIF 退化为
  首帧，已知限制）。Rust 端读剪贴板无焦点要求，**不走**前端
  `navigator.clipboard.read()`（非 key window 不可靠）。
- **输入框 `Cmd+V` 粘图片**：input 挂 `paste` 监听，`clipboardData.items`
  含 `image/*` 改走 `paste_from_clipboard`；已键入文字作为图片 todo
  标题（无标题即纯图 todo 占满整宽）。仅 input 聚焦时触发（未聚焦
  浮窗仍由粘贴按钮兜底）。
- **QuickLook 预览窗**（`preview.html` + `src/preview.ts` +
  `src/preview.css`）：独立动态 webview，无边框透明 + always-on-top +
  `acceptFirstMouse`，定位浮窗**默认左侧优先**（放不下放右侧），
  尺寸按附件宽高比或文本预测量算。**hover 任意卡 80ms 微防抖后立即
  出预览**（取消「截断/图片卡才预览」门控）；预览已开 hover 换卡 =
  0ms 立即切换。文本卡前端 offscreen measurer 预测量**一次开到位**
  （show 后不再 resize）。点击缩略图 → `pinned=true` 聚焦编辑态。
- **预览窗可编辑**：textarea 防抖 700ms 自动保存（`update_todo`），
  空标题不保存；切 todo / 关窗前 `flushPendingSave` 强制落盘；外部
  `todos-changed` 在 textarea 未聚焦时回填，todo 被删 → 自关。
- **预览窗 footer**：左下 创建日期（`created_at`，epoch ms → locale
  短日期）｜中 hint（flex:1 可缩省略号）｜右下 完成日期（done 任务
  取 `updated_at`，翻 status 时后端刷新）+ 复制按钮 + 垃圾桶删除按钮
  （二次确认实红同卡内语义）。新增 i18n key `preview.created` /
  `preview.completed`。
- **删除 todo 后 8s 内可撤销**：删除仍是硬删除，但附件文件不再随
  `delete_todo` 立即删。前端单条 undo 栈暂存被删完整 Todo + 显示带
  「撤销」按钮的 action flash（复用 `.mini-flash.has-action`，强调
  色 `--c-data-info`）。点撤销调 `restore_todo` 恢复（图片完整可
  恢复）；超时调 `purge_attachment` 真删附件文件（路径穿越校验
  禁 `/ \ ..`）。
- **附件延迟删除 + 启动孤儿扫描**：`delete_todo` 不删附件文件；
  前端 undo 栈超时调 `purge_attachment` 真删；启动
  `Store::purge_orphan_attachments` 扫 `attachments/` 目录兜底崩溃
  / 异常退出残留。
- **删除事件驱动统一入口**：`delete_todo` emit `usticky://todo-deleted`
  （被删完整 Todo），主浮窗 listen 后统一管 undoEntry + flash。
  预览窗 `deleteSelf` 直接调 `delete_todo` 也走这条路径 → 预览窗
  删除也有撤销入口，不再需要各调用点各自管 undo。
- **系统托盘归位按钮**：托盘右键菜单新增「Reset Position / 归位位置」
  顶层项（`tray.rs` `MenuItem::with_id("reset_position", ...)`），与
  设置面板归位按钮复用同一个 `_core` 路径
  （`commands::reset_floating_window_core`）—— 移到主屏正中央 +
  持久化 + emit `usticky://window-pos-changed`。
- **MIT LICENSE**：仓库首次声明许可证。
- 新增 i18n key：前端 `app.action.paste/preview` + `app.paste.*` +
  `preview.*`；后端 `commands.paste.image_title` + `window.preview`。
- **PERF_AUDIT.md + `pnpm perf:baseline` 脚本**：仓库首次加入性能
  审计文档 + 100/500/1000 todo 渲染基线测量。

### Changed

- **预览窗状态机重写**：hover 任意卡 80ms 立即出（取消「截断/图片卡
  才预览」门控，`isTitleTruncated` 已删）；预览已开 hover 换卡 =
  0ms 立即切换（`openPreviewFor` 直调，不走 dwell）。**自动驻留**
  （鼠标在某窗上）vs **显式 pin**（用户点击缩略图 / 预览窗真聚焦）
  拆成两个独立状态 —— "路过"不会把内容锁死。
- **预览定位跟手**：前端传 `anchorY = card.getBoundingClientRect().top`
  （视口相对逻辑 px），Rust 换算物理屏坐标 → 预览顶对齐被 hover
  卡片顶；左右策略默认卡左，左边空间不足放右边（上下方向砍掉）。
- **预览关闭走 grace close**：unhover / hover(false) / 浮窗空白区 /
  preview-left 四处共用 450ms grace close（`schedulePreviewClose`
  统一入口）—— 鼠标穿 card↔preview 缝隙的"两窗都不在"瞬态不再
  闪断跟手体验。hover(true) / over_preview / hoverCard 都会 cancel。
- **预览定位 clamp 用 `monitor.work_area()`**：macOS visible frame
  （扣菜单栏+Dock）；Win 扣任务栏。贴底 hover 长卡时预览底边不再
  滑进 Dock。
- **macOS 浮窗面板显示走 `orderFrontRegardless`**：新增
  `platform::show_window_no_activate`（macOS 走 `orderFrontRegardless`，
  Win/Linux 直接 show）—— hover 路径（pinned=false）的复用 show
  改走它，悬浮面板永不偷焦点。
- **persistence 写锁内原子快照 + 代际**：避免多任务并发写时旧快照
  覆盖新数据（丢失更新）。`Store` 内部记录 generation，写锁内同时
  取快照 + 读 generation，落盘前再次比对。
- **Geom persist 改后台 trailing debounce**：`GEOM_NOTIFY: OnceLock<Notify>`
  + 后台 `geom_persist_loop` select 模式，每像素 spawn 持久化改为
  200ms trailing debounce + select 模式。
- **single-instance 双开 raise**：第二次启动由
  `tauri-plugin-single-instance` 拦截 → raise 主实例浮窗
  （PinBottom 默认 mode 下裸 show 会停 level=-1 被盖住，路径走
  `quick_show_floating_window` 先 raise 到 FLOATING 再 show）。
- **退出 flush**：todos.json 持久化路径统一，无残留未写状态。

### Fixed

- **粘贴图片后 todo 内显示空白框**（双层根因）：a) `get_attachments_dir`
  在 `init()` 里排在 `get_todos` 之后，首屏 `buildTodoRow` 调
  `attachmentUrl()` 时 `attachmentsDir` 仍 null → `<img>` 无 `src` →
  空白边框框（且无 error 事件 → `thumb.remove()` 不触发，框常驻）；
  b) 缩略图 20×20 太小。修：`get_attachments_dir` 挪到 `get_todos`
  之前；卡内图片改**内联单行**（`flex:1 1 0` 跟 `.todo-title` 1:1
  共享宽度；纯图 todo 标题折叠让图片独占整宽）。
- **预览 ~1Hz 闪烁**：v0.2.1 JS heartbeat 1200ms toggle
  `.force-reflow` 强制 backdrop 重采样 → 视觉上每秒闪一次。换成浮窗
  同款连续心跳动画（preview.css `pv-backdrop-heartbeat`：rotate
  0.001°/opacity 0.001 亚像素微动，compositor-only，linear 4s
  infinite —— 永远在动 → layer 不进 2s 节流窗口，且无 toggle 边沿
  不可见）。JS heartbeat 三处全删。**教训：backdrop 保活只能用
  "永远在动"的连续动画，任何周期性 class/属性 toggle 都是可见闪烁**。
- **预览窗外圈黑边**：macOS 原生窗口阴影（builder `.shadow(true)`）
  在透明无边框窗上画一圈硬黑边。改 `.shadow(false)`，柔和投影由
  preview.css `--pv-shadow` 负责。
- **hover 锁死**（鼠标进预览窗再回列表，预览不更新）：预览窗一旦
  拿焦点（`acceptFirstMouse` 点击 / `show()` 抢 key），WKWebView
  恢复派发 mouseenter → `preview-entered` 把 pin 置上 → 永不释放。
  修：pin 改焦点语义 + show 改 `orderFrontRegardless` 不抢 key
  （hover 路径双保险）。
- **预览窗被 Dock 遮挡**：定位 clamp 用 `monitor.size()`（全屏物理
  分辨率）→ 改 `monitor.work_area()`（macOS visible frame，
  扣菜单栏+Dock；Win 扣任务栏）。
- **预览开着时玻璃丢毛玻璃**：macOS WKWebView backdrop-filter 在
  新 always-on-top 窗口上屏后失效。修：open/close preview 两条路径
  末尾 emit `usticky://backdrop-refresh` 救浮窗；preview.ts 持续心跳
  给 .preview-panel 挂亚像素微动（prewarm 隐藏态跳过不耗 GPU）。
- **预览 2 秒丢毛玻璃（跟浮窗一起丢）**：v0.2.1 加的 JS heartbeat
  元凶改为浮窗同款连续心跳动画（见上）。
- **多卡 hover 计数不准**：hoverCard 状态机重复挂 `.card-hover` /
  unhoverCard 漏解冻；`setCardHover` 改为幂等，统一 hoverCard /
  unhoverCard / over_preview 三处 .card-hover 增删。
- **预览窗 PENDING_PREVIEW_TODO 竞态**：prewarm 隐藏创建的 webview
  可能没加载完，open_preview_window reuse 路径 emit 的 preview-todo
  丢失 → 后端 emit 前先存 `PENDING_PREVIEW_TODO`，preview.ts init
  末尾 listeners 就位后 `take_pending_preview_todo` 主动取一次。
- **预览 footer hint 截断**（"Press ⌘" 后半截消失）：flex item
  默认 `min-width:auto` → input 不肯缩到 placeholder 内容宽以下，
  hint 被推出瓦片右缘。改 `.todo-input input { min-width: 0 }`；
  `NARROW_THRESHOLD` 280→240（完整 `⌘⇧Space` 在 240 宽也算得过来）。
- **预览 2 个 bug（preview+card）**：预览窗离开不关（unhover 不会
  触发 grace close）+ 宽图挤按钮（图片 todo thumb 宽挤压 actions
  按钮）。修：unhover 即触发 `schedulePreviewClose` 统一入口；
  `setCardHover` 冻结 .todo-thumb 当前宽（无 actions 的 1:1 宽），
  actions 出现时只有 .todo-title 收缩省略。
- **预览状态机 bug**：`cancelPreviewClose()` 顺序修正 —— 提到
  `previewTodoId === id` 短路**之前**（旧代码 unhover 排的 grace
  close 在"快速回同一张卡"时照样炸，预览 hovering 中被关）。
- **预览窗 drop defer-lower**：unhover 后预览立即关 + 卡片立即
  失强调（撤 defer-lower）+ 窗口降级与预览消失同帧。
- **预览 4 向定位**：`preview_position()` 左→右→上→下，第一个
  不与浮窗 rect 相交且在显示器内的方向胜出；兜底右侧 clamp 允许
  重叠（over_preview-inside 已兜住振荡）。坐标系全物理像素 top-left
  origin，"上方" = y 减小。
- **预览 backdrop-refresh 频繁刷**：v0.2.1 每次 hover 换卡都刷 →
  浮窗整窗 filter repaint → 用户看到的"闪"主要来源。改为：
  `backdrop-refresh` 只在隐藏→上屏（prewarm 首显 / 全新创建）和
  关闭时 emit；可见窗口换内容/跟手移动不再刷。
- **预览 footer 创建/完成日期不随 locale 重渲染**：preview.ts
  `loadTodo` 加 generation 防过期回调；日期渲染挂 locale listener。
- **预览 blur 未 flush**：textarea 防抖 700ms 在 blur 时未强制落盘
  → 切走 app 时正在编辑的修改丢失。修：blur handler 同步调
  `flushPendingSave`。
- **预览 2 个 bug**：hover 离开后预览立即关 + 卡片立即失强调
  （撤 defer-lower）+ 窗口降级与预览消失/卡片失强调同帧
  （`de255ae`）。
- **预览 hover 跟手四处共用 `schedulePreviewClose`**：统一入口
  修状态机 bug。
- **preview PENDING_PREVIEW_TODO race**：preview `PENDING_PREVIEW_TODO`
  + `take_pending_preview_todo` 兜底 prewarm 竞态。
- **cross-verified code review 修复 10 个 bug**（`441d7b4`）：涉及
  macOS dispatch_failed recovery 不无条件 LOWER、quick-add
  `compare_exchange` 原子化、Win z-order `APPLY_Z_ORDER_LOCK: Mutex<()>`
  锁、Win scale_cache 跨 DPI 屏 refresh、tray menu 打开时
  `MENU_OPEN_FLAG` 跳过 rebuild、`rebuild_tray` 改 `try_state`、
  `reset_floating_window` 用持久化 (w,h) / 检查 `is_visible` /
  emit `window-pos-changed`、`set_app_locale` 白名单、`update_todo`
  no-op 短路、`persist_to_disk` mode 0o600 + rename 后 fsync 父目录。
- **Rust 全量审查 P0-P3 修复**（`e01200e` + `daa20cc` + `6348091` +
  `1d7a4b4`）：平台层 mac/win hover 与 z-order + 数据/壳/托盘层
  全量修复 + 前端浮窗+设置+i18n 修复 + CI/脚本/capabilities/cargo
  修复。
- **rowStableRefs 取消 `title` 缓存**：编辑模式
  `titleEl.replaceWith(input)` + `exitEditMode` 新建 `.todo-title`，
  缓存会持有 detach 旧节点 → 后续 render 写到不可见旧节点 = 预览窗
  编辑后浮窗视觉不同步（用户 2026-08-14 实测）。改回每次
  `querySelector`，匹配 thumb / due 的「动态节点按需查」模式。
  **规则：缓存 DOM 子节点必须确认永不被 replace**（见
  [[usticky-row-cache-stale-title]]）。
- **设置面板快捷键录入接受非 ASCII 单字符键**（如 `F12` / `A` 单键
  触发 quick-add）：改 parse 后 `mods.intersects(CONTROL|ALT|META|
  SUPER)` 否则 Err。
- **设置面板 `Cmd+Z` 占位 handler 删除**（v0.2 候选）：handler 未实现，
  删免误导。
- **macOS PinBottom hover 切换不闪烁**：原 FIFO 主线程队列先
  LOWER 再 RAISE → 闪烁。改：悬停时 BELOW_NORMAL 延 50ms + 条件
  skip（`LAST_INSIDE` 仍 true → 跳过 LOWER）。
- **macOS 隐藏门控 `LAST_INSIDE` 复位 + deferred 模式校验** +
  SLOT 代际（`5b1b7e8`）。
- **Windows hover 进入边沿抖动**（throttle 跳过 + 隐藏重置 +
  scale 缓存收紧，`149b111`）。
- **浮窗拖拽 SortableJS floating fallback 阻断**：用 `forceFallback
  + fallbackOnBody` 时克隆脱离 `#app` CSS 变量作用域，退化成透明
  底裸文字盖在最顶层。改 `.todo-card.sortable-drag /
  .sortable-fallback { visibility: hidden }`。
- **image mousedown 不能拖动整张卡**：撤 `.todo-thumb` 拖拽拦截，
  改为只拦 image 内部 click。
- **i18n 死 key 清理**：删后端 `error.empty_title` / `error.not_found`
  / `error.too_long`；前端 `tray.show` / `tray.hide` /
  `app.action.edit` / `app.empty.done`。
- **preview 关闭强制路径**：新增 `close_preview_window({force:false})`
  命令以处理关闭失败情况（w.close() 在 Tauri 2 / WKWebView 下可能
  不触发 webview beforeunload）。`force:false` = 预览窗正聚焦（编辑中）
  则不关，blur 后自治关闭。
- **preview 4 bug 合并**：常驻置顶 + Esc 无效（grace close 不重排，
  `af8b6e4`）+ 预览窗离开不关（unhover 不触发 grace close）+
  宽图挤按钮。

### Performance

- **玻璃背景搬到 `::before` 伪元素 + opacity 过渡**（2026-08-13 perf）：
  拆 `--tile-bg-rgb`（固定色）+ `--tile-bg-alpha`（透明度）两个
  变量；背景从 `.todo-card` 移到 `::before` + **opacity 过渡替代
  background-color 过渡**（opacity 由 GPU 插值，compositor-only 零
  重绘；background-color 走 paint，每帧重绘 N 层 ::before，todo
  一多 hover 过渡就掉帧卡顿）。`::before` 独立合成层靠
  `transform: translateZ(0)` 强制提升；`overflow:hidden` +
  `border-radius:inherit` 把 ::before 裁到卡片圆角内，避免 1px 露出
  错位。视觉等价：rgb(28,30,38) × opacity 0.30 ≡ rgba(28,30,38,0.30)。
- **`glass blur 28 → 10px`**：与 Musage 浮窗 idle 观感对齐（视觉
  一致 + WKWebView backdrop throttling 2 秒丢失风险降低）。
- **`tile-saturate 120% → 180%`**：玻璃色与 Musage 浮窗对齐。
- **render 改增量 DOM diff**：从全量重建改为增量 DOM 复用，
  `rowStableRefs` 缓存稳定节点（check / actions / copy / del）首次
  `buildTodoRow` 一次性查好；动态节点（thumb / due）按需
  `querySelector` —— 它们的存在/缺失随 todo 内容变化，缓存会 stale
  不划算。**`title` 不缓存**（见 Fixed 段）。
- **预览窗加载优化**：preview webview 复用（已开不重建）→ 切 todo
  / resize 走原地 emit + JS resize，避免 reload 丢编辑内容。
- **IPC 往返优化**：预览窗 hover 状态改前端本地
  `mouseenter`/`mouseleave` 优先（聚焦时），未聚焦才走 Rust hover
  emitter；preview.ts 渲染时不每帧 invoke 持久化。
- **dump_perf 路径**：Tauri command `dump_perf` 默认 no-op（避免
  生产环境未使用开销）；CI 性能测量时显式 enable。
- **Paste 按钮独立玻璃框**：`ensureInputBar` 重构为 `.todo-input-row`
  flex 行容器，左 `.todo-input` 右 `.todo-paste-btn`（独立瓦片，
  `align-self:stretch` 与输入框等高方形）；心跳 / force-reflow /
  backdrop-refresh 三个选择器都补 `.todo-paste-btn`。

### Security

- **附件落盘 `mode 0o600`**（对齐 todos.json 安全姿态）：
  `OpenOptions::mode(0o600)`，不再世界可读 tmp。
- **删除附件路径穿越校验**：`purge_attachment` /
  `purge_orphan_attachments` 禁路径含 `/ \ ..` —— 撤销恢复 todo 时
  附件文件名不能诱导删仓库外文件。
- **剪贴板 size cap 先 `data.len()` / metadata 再 alloc**（防 OOM）：
  GIF / TIFF / 普通 PNG 在 alloc 前先看 metadata 头部尺寸上限，超过
  25MB 直接拒。
- **快捷键必须带 modifier**：parse 后
  `mods.intersects(CONTROL|ALT|META|SUPER)` 否则 Err。
- **clipboard paste 0o600 + 路径穿越**：见上。
- **attachment path 校验**：恢复 todo 时附件 file 字段仅取 basename
  + 强制后缀白名单（jpg/jpeg/png/gif/webp）。

### Internal

- **CI 工作流权限裁剪 + 钉版本 + verify always**
  （`f603bc7`）：所有 action SHA 钉死 + Verify job polling 循环取代
  `sleep 15` + ci.yml / release.yml 权限只读 / 写 artifacts。
- **scripts/verify-versions.cjs 抽取**（`afa4338`）：原 release.yml
  inline `node -e` 抽成跨平台 Node 脚本（CI 跨平台兼容）。
- **release.yml Sync version 移到 pnpm install 之后**：避免 sync
  路径上的 pnpm 解析失败。
- **capabilities 裁剪到最小集**（`1d7a4b4`）：删冗余
  `opener:default` + 7 个未调用的 `core:window:allow-*`。
- **`staticlib` crate-type 删**：CI 是 MSVC，desktop 用 rlib 足够。
- **AGENTS.md 修正 stale claim**：`b9d0fcd`（v0.1.2 P3-4 fix 描述
  是 stale claim；`render()` 实际是全量重建；前端 `tray 全文案`
  claim 过期）+ `d5d3b6f`（删除撤销 v0.2.5 changelog + 修附件
  删除过时描述）。
- **依赖收窄**：删未用 `plugin-notification` / `plugin-autostart`
  JS 依赖（v0.2 不依赖这两个 plugin）。

## [0.2.0] - 2026-07-21

首个公开发布版。含 v0.1.2 之后的全部交互修复与新功能，以及 CI/CD 流水线。

### Added

- **复制按钮**（删除键左侧）：一键复制 todo 文本，mini-flash 反馈；
  `navigator.clipboard.writeText` + `execCommand` 双路径兜底
- **删除二次确认**：第一次点击进入确认态（实红 + tooltip 提示），
  3s 内第二次点击才真删；超时 / hover 结束自动撤销
- **GitHub Actions 流水线**：CI（前端构建 + 三平台 cargo check）+
  Release（macOS arm64/x64 dmg、Windows NSIS、Linux AppImage/deb，
  tag 触发自动出 draft release）
- 新增 i18n key：`app.action.copy` / `app.action.confirm_delete` /
  `app.copy.flash` / `app.error.copy_failed`

### Changed

- **hover 背景与 Musage 逐项对齐，两 app 同屏颜色统一**：hover
  `--tile-bg` 0.92 → 0.82、`--tile-border` 0.12 → 0.10、`--tile-shadow`
  0.50/0.05 → 0.45/0.04（此前 Usticky hover 比 Musage 深一档，同屏时
  玻璃色不一致）。idle→hover 差随之与 Musage 相同（0.30 → 0.82 =
  0.52 alpha）。blur 28px / saturate 180% 两边本就一致，不动。
- **idle（未 hover）背景压到 Musage 档位，hover 才显现**：`--tile-bg`
  alpha 0.55 → 0.30、`--tile-border` 0.06 → 0、`--tile-shadow` → 0
  （对齐 Musage idle：0.30 alpha / border 0 / shadow 0），idle 几乎只剩
  文字浮在桌面上，hover 才显出玻璃瓦片（idle→hover 差 0.62 alpha）。
  done 卡 idle 同步 0.24 → 0.12 保持"更折叠"相对关系。blur 仍 28px
  写死不跟 Musage 的 10px —— 那是 WKWebView backdrop throttling
  三层防御的前提。低电量模式（锁全强度）不受影响。
- **拖拽时 checkbox 变 ⇅ 上下指示符**：旧版拖拽中 `.todo-check` 被
  `display:none` + `padding-left:11px`，文字左挤。现在 checkbox 圆圈
  在 dragging / sortable-chosen 态变成 ⇅（占住原槽位，文字不位移），
  同时明示"正在拖的是这张"；delete / due 标签拖拽中仍隐藏。
  done 卡拖拽同样显示 ⇅（`#app` 前缀压过 done 的绿底 ✓）。
- **清理网络 entitlement 对齐"不联网"产品承诺**：删除
  `entitlements.plist` 的 `com.apple.security.network.client`。
  Usticky v0.1 实际零网络请求（前端静态 import + CSP 禁非 self/ipc
  connect），保留 entitlement 只会扩大 Hardened Runtime 攻击面。
  v0.2 加 Tauri updater 时再加回来。AGENTS.md / README 的"不联网"
  承诺现在跟二进制实际权限一致。
- **卡内按钮压缩 + 不占位**：复制/删除按钮包进 `.todo-actions` 容器
  （22→18px、组内 gap 10→2），默认 `display:none`，hover 卡片才显示
  —— 未 hover 时 title 不再被白挤 ~46px

### Fixed

- **未聚焦时 hover 卡片无交互**（删除按钮不显示、长文不展开，必须
  点一下聚焦才生效）：根因是 hover-pos 屏幕坐标→视口坐标换算链
  （tao 单位混用 + `window.screen` 基准屏假设）在部分机器上错位，
  `elementFromPoint` 恒 null。**换算挪到 Rust 端同坐标系直接相减**
  （macOS 用窗口自身高度翻 Y 轴，Win 除 scale），前端拿到直接用
- **未聚焦时 hover 按钮无反馈**（不变色、光标不变手型）：按钮
  显隐/反馈弃用 CSS `:hover`（非 key window 不激活且 stuck），
  统一 `.card-hover` / `.btn-hover` class 状态机（JS mouseenter +
  Rust hover-pos 双路径驱动）；手型光标走 `set_cursor_pointer`
  命令 → macOS `NSCursor` 兜底
- **未聚焦时点按钮要点两次**（第一击被 click-to-focus 吞掉）：
  `acceptFirstMouse: true`，点一次复制键即触发
- **多张卡 × 删除按钮概率残留**：`:hover` stuck 所致，显隐改单一
  `.card-hover` 状态机后实测穿梭 9 卡计数恒 ≤1
- **拖拽 todo 卡时浮动克隆遮挡内容**：SortableJS
  `forceFallback + fallbackOnBody` 会把被拖卡克隆一份 append 到
  `document.body`，以 `position:fixed + z-index:100000` 跟随光标；
  且克隆脱离 `#app` 的 CSS 变量作用域，卡片外观变量全部失效，
  退化成透明底裸文字盖在最顶层。落点本已由列表内 `.dragging`
  占位卡实时演算展示，浮动克隆纯属冗余 —— 直接
  `.todo-card.sortable-drag / .sortable-fallback { visibility: hidden }`
  整体隐藏（visibility 无内联样式冲突，克隆保留盒模型，不影响
  SortableJS 内部 transform 更新 / drop 移除逻辑）。
- reset_floating_window 加 `available_monitors().first()` fallback
  （Wayland 等场景下 `primary_monitor()` 返 None）+ tracing 日志
  输出目标显示器。

## [0.1.2] - 2026-07-06

hover emitter + 设置面板 + tray 子菜单 + 三档 pin mode + SortableJS 拖拽
（累计 v0.1.1 / v0.1.2 两轮迭代）：

#### v0.1.0 骨架（2026-07-02）

- Tauri 2 项目结构 + 双 locale i18n（en + zh-CN）+ iOS 26 玻璃质感 CSS + 浮窗位置/尺寸自动记忆
- IPC 接口：`list` / `add` / `update` / `delete` / `reorder` + `get_app_locale` / `set_app_locale`
- 全局快捷键 `CmdOrCtrl+Shift+Space` → `usticky://quick-add` → 聚焦 input
- JSON 持久化：原子写（tmp → rename）+ Unix 0600 + 解析失败 backup `.bak.<ts>`
- `WindowEvent::Moved` / `Resized` → spawn 异步任务持久化（不阻塞 UI 线程）
- 关闭 = 隐藏（`api.prevent_close()` + `window.hide()`），tray 左键单击切换显隐

#### v0.1.1（2026-07-02，搬 Musage 三档 pin mode）

- `PinMode` enum（PinTop / PinBottom / Normal）+ 持久化到 `todos.json`
- macOS：`NSWindow.setLevel` 切三档（`kCGFloatingWindowLevel` / `kCGNormalWindowLevel - 1` / `kCGNormalWindowLevel`）
- Windows：`HWND_TOPMOST` / `HWND_BOTTOM` / `HWND_NOTOPMOST` dual-path（`SetWindowPos` + `SetWindowLongPtrW` 改 `WS_EX_TOPMOST` style bit）
- Linux：no-op stub（`set_always_on_top(true)` 已是最实用方案）
- `get_pin_mode` / `set_pin_mode` 命令 + `usticky://pin-mode-changed` 事件

#### v0.1.2（2026-07-03 → 2026-07-06，hover emitter + 设置面板 + tray 子菜单）

- **Hover emitter**：50ms tick 全局鼠标轮询
  - macOS：`NSEvent.mouseLocation` + `NSWindow.windowNumberAtPoint` 命中测试（不仅检查鼠标在 frame 内，还确认浮窗是该点 topmost）
  - Windows：`GetCursorPos` + `WindowFromPoint` + `GetAncestor(GA_ROOT)` 命中测试
  - Dwell-time hysteresis（enter 3 ticks / exit 2 ticks）防边缘抖动振荡
  - 永远 emit `usticky://floating-hover`（驱动 CSS `body[data-hover]` 玻璃效果，不分 pin mode）
  - PinBottom 模式额外切 NSWindow level / Win z-order（hover 临时置顶）
- **Hover 双路径**：Rust emit + JS `mouseenter`/`mouseleave` 40ms debounce；失焦时主动 `setHoverAttr(false)` 清 stale state；visibilitychange 清理
- **SortableJS 拖拽排序**：pending / done section 各一个 Sortable 实例，`onEnd` 批量 `reorder_todos`
- **标记完成动画**：`.vanishing` class + 300ms 延迟后才调 IPC，失败回滚 class
- **设置面板**（`settings.html` + `src/settings.ts` + `src/settings.css`）：单页设计
  - 浮窗层级（pin mode）segmented control
  - 语言切换（en / zh-CN）
  - 浮窗归位到主屏幕正中央（`reset_floating_window`）
  - 关于（版本 / 产品名 / GitHub）
- `open_settings_window` 命令：动态创建 webview（已开则 focus，关闭时 destroy，不在 tauri.conf.json 常驻）
- **Tray Settings 子菜单**：pin mode 三档 `CheckMenuItem`（带 checkmark）+ "Open Settings Panel..."
  - locale / pin mode 切换时 `rebuild_tray` 走 `run_on_main_thread` 派发避免 NSStatusBar `assertBarrierOnQueue` SIGTRAP
- **Tray icon 改 U 字母**（`scripts/generate_icons.py`）：白底圆角 + 黑色加粗 U + ring 装饰；每个尺寸原生渲染（不降采样）；macOS 用 `iconutil` 拼真 `.icns`
- 浮窗控制命令：`reset_floating_window` / `resize_floating_window` / `hide_floating_window` / `show_floating_window`
- `set_floating_hover_raise` 命令（前端兜底信号，macOS/Win 上 tracker 已自行处理，此处 no-op）
- **locale 切换链路**：tray + settings 窗口 title 同步重建（`usticky://locale-changed` listener）
- **Pin mode 跨 webview 同步**：`usticky://pin-mode-changed` listener 在浮窗 / 设置面板 / tray 三处生效
- `persist_and_emit` 失败时 emit `usticky://persist-failed`（不再静默吞掉，前端 mini-flash 提示）
- 启动时恢复浮窗位置 clamp 到主显示器范围内（防副屏拔了之后窗口扔到屏幕外）

### Fixed（v0.1.2 调试期间）

- PinBottom 模式 hover 误置顶 + 毛玻璃效果振荡消失（dwell-time hysteresis 阈值改回 Musage 的 3/2）
- hover 玻璃效果在 transparent 区域消失（改回 `windowNumberAtPoint` 命中测试，不用 `frame.contains`）
- hover 玻璃效果在启动时丢 —— 必须点一下才生效（去掉 `!focused` 守卫，un-focused 浮窗的合法 hover 不该被吞）
- hover 玻璃效果在切回浮窗时丢（`setHoverAttr(false)` 重置 dedup 状态）
- 光标移上浮窗时闪烁（hover emitter 同值去重 + JS 40ms debounce）
- 浮窗位置 `set_position` 在副屏拔了之后扔到屏幕外（启动时 clamp 到主显示器范围）

## [0.1.0] - 2026-07-02

### Added

- 项目初始化（forked from Musage v0.2.0 浮窗经验）
