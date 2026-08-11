# Usticky vs Musage 性能差距 · 修复路线图

> 全量性能审查报告（2026-08-11，v0.2.6 基线；保留 2026-08-06 草稿并附当前工作树校正）。方法：4 域只读广扫 → 逐候选量化 → 对照 Musage 结构性差异 → WebKit/Tauri 官方资料佐证。本报告是诊断 + 路线图，不含落地代码。

## TL;DR

Usticky **不比 Musage 慢在玻璃或 hover tick 上**——这两块 Usticky 反而更轻（blur 10px vs Musage 28px、前端零 `setInterval` 轮询）。差距集中在三个 Musage 没有的新层和一个启动退化：

1. **预览窗 `render()` 每次 hover 换卡都 `innerHTML=""` 全量重建**（每次约 10-12 个节点、7 个监听器，视图片/done 状态而变），是 hover 跟手的首要候选——Musage 没有预览窗，无可比性。
2. **首屏 5 次 `await invoke()` 串行**，其中 locale 之后的 4 个读取彼此独立，却排队等 IPC 往返；并行后关键路径的串行深度可从 5 段降到 2 段，且当前 `create_dir_all` 也在首屏路径。
3. **每张卡 10 个 `addEventListener`**（check/title/copy/del × click + 各自 mouseenter/leave），N=100 时首屏要绑约 1000 个监听器；Musage 的委托主要覆盖错误卡片和空态 CTA，不是全部 provider 卡片动作。
4. **hover-pos 监听约 20Hz 调 `elementFromPoint`**（Musage 无此调用），这是 Usticky 独有的 IPC + JS hit-test 热路径；是否造成同步布局要由 Web Inspector profile 证明，不能仅凭静态代码断言。

优先基准 S1/S2/S3；只有实测命中后再实施。静态审查能够给出工作量变化，不能预先承诺一定持平或快于 Musage。

---

## 收益排序表

| # | 瓶颈 | 位置 | 预期收益 | 改动成本 | 与 Musage 差距 |
|---|---|---|---|---|---|
| S1 | 首屏 5 次 IPC 串行 | [main.ts:2060-2090](file:///Users/wyh/Project/Usticky/src/main.ts#L2060-L2090) | startup invoke 串行深度 5→2；wall time 以 20 次冷启 p50/p95 验证 | 低（~15 行） | Musage 首屏 invoke 更少 |
| S2 | 预览窗全量重建 | [preview.ts:88-186](file:///Users/wyh/Project/Usticky/src/preview.ts#L88-L186) | 每次换卡约 10-12 个节点/7 listener 重建→稳定骨架仅更新 0-2 个可选节点 | 中（~120 行重构） | Musage 无预览窗（Usticky 独有开销） |
| S3 | hover-pos 未 rAF 合并 + 高频 hit-test | [main.ts:1907-1990](file:///Users/wyh/Project/Usticky/src/main.ts#L1907-L1990) | 静止 10 秒最多约 200 次事件；先以计数和 handler wall time 确认收益，再决定 rAF/去重 | 低（~20 行，需验证交互） | Musage 无 elementFromPoint 调用 |
| A1 | 逐卡 10 个 addEventListener | [main.ts:648-712](file:///Users/wyh/Project/Usticky/src/main.ts#L648-L712) | N=100 确实约 1000 个 listener；具体构建时间/内存需 profile，不预报固定节省 | 中（事件委托改造） | Musage 的委托主要覆盖错误卡片/空态 CTA，并非全部 provider 卡片动作 |
| A2 | updateTodoRow 每行 7 次 querySelector | [main.ts:731-840](file:///Users/wyh/Project/Usticky/src/main.ts#L731-L840) | 稳态每次 render 7N 次查询→缓存 refs 后接近 0；实际 wall time 需测 | 低（WeakMap 缓存，~30 行） | Musage updateCard 同样逐行 querySelector（非回归，但仍可优化） |
| A3 | addTodo 等 fsync 才清空 input | [main.ts:2474](file:///Users/wyh/Project/Usticky/src/main.ts#L2474) + [commands/mod.rs](file:///Users/wyh/Project/Usticky/src-tauri/src/commands/mod.rs) | 当前清空等待 1 次完整 persist；乐观路径可先清空，但需保留失败回滚/耐久性 | 中（乐观更新+回滚） | Musage 无文本输入路径 |
| B1 | 预览窗 18px 大面积 blur 与浮窗并存 | [preview.css:42](file:///Users/wyh/Project/Usticky/src/preview.css#L42) | A/B 比较 18px 与 10-12px 的 Composite/Paint p95；静态审查不预报 GPU 百分比 | 极低（1 行实验） | Musage 无第二窗口 |
| B2 | setCardHover 在 hover 边沿读 offsetWidth | [main.ts:968,976](file:///Users/wyh/Project/Usticky/src/main.ts#L968) | 消除一次边沿强制同步布局 | 低（CSS-only 或预测量） | 同类模式 |
| B3 | wheel 每事件读 scrollHeight/clientHeight | [main.ts:1438-1445](file:///Users/wyh/Project/Usticky/src/main.ts#L1438-L1445) | 滚动时省 60-120 次/s 布局读 | 极低（节流/rAF） | Musage 同款，非回归 |
| B4 | persist 用 pretty JSON + 每次全量 fsync | [todo.rs](file:///Users/wyh/Project/Usticky/src-tauri/src/todo.rs) | 先测 pretty/compact 实际字节数和 serialize/fsync p95；不得默认删除 durability fsync | 低（实验后决定） | Musage 配置保存有 debounce |
| B5 | get_attachments_dir 启动即 create_dir_all | [commands/mod.rs](file:///Users/wyh/Project/Usticky/src-tauri/src/commands/mod.rs) | 首屏少一个 syscall | 极低（懒加载） | — |
| B6 | Win 隐藏态 hover emitter 仍 4 次 Win32/tick | [platform/windows.rs](file:///Users/wyh/Project/Usticky/src-tauri/src/platform/windows.rs) | 目标是跳过每 50ms 的 Win32 hit-test；线程仍会 sleep/wake，不能写成 CPU 归零 | 极低（加可见性 gate） | macOS 已有 gate，Win 漏了 |

---

## 分级详解

### S 级候选（优先测，命中后预期最可感知）

#### S1 · 首屏 5 次 IPC 串行 → 并行 + 批处理

**现状**（[main.ts:2060-2090](file:///Users/wyh/Project/Usticky/src/main.ts#L2060-L2090)）：
```
await initLocale()                  // 1 次 invoke
currentShortcut = await get_shortcut // 2
currentPinMode  = await get_pin_mode // 3（彼此独立）
attachmentsDir  = await get_attachments_dir // 4（含 create_dir_all syscall）
const snap = await get_todos        // 5（数据最大头）
render(snap)
```
locale 初始化之后的 4 个读取彼此独立，但当前仍串行排队。

**量化**：当前 startup invoke 串行深度为 5；保留 locale 前置、其余 4 个读取并行后可降为 2。`get_attachments_dir` 还确定会触发一次 `create_dir_all`；具体 wall time 必须由冷启动 p50/p95 给出。

**改法**：
- `pin_mode` / `attachments_dir` / `todos` 三个无依赖请求 `Promise.all`；
- `get_quick_add_shortcut` 只用于输入框 hint 文案，可延后到首帧渲染后再取（不挡首屏）；
- 或后端加一个 `get_app_state()` 聚合命令，一次 IPC 返回所有启动配置，省掉 3 次 IPC 往返头开销。

**为什么是 S 级候选**：直接命中"3 秒唤出"关键路径，且操作数可明确下降；是否达到可感知收益仍需冷启动基准。

**验证**：在 `init()` 首行 `performance.mark('boot-start')`，每个 invoke 前后与首次 `render()` 后打 mark；改前改后各测至少 20 次冷启，报告 p50/p95。

---

#### S2 · 预览窗 render() 每次换卡全量 innerHTML 重建

**现状**（[preview.ts:88-186](file:///Users/wyh/Project/Usticky/src/preview.ts#L88-L186)）：
每次 `loadTodo`（hover 换卡 / preview-todo 事件）都执行：
```ts
appEl.innerHTML = "";   // 拆毁约 10-12 个节点 + 7 个监听器（视状态而变）
// 重新 createElement panel/img/textarea/footer/created/copy/del ...
// 重新 addEventListener × 7
// 图片卡：new Image() → src=asset:// → 重新走 asset 协议 + 解码
```

**量化**：每次 render 确定创建约 10-12 个节点和 7 个 listener；稳定骨架可把换卡降为文本/属性更新，并只增删图片、done 日期等 0-2 个可选节点。图片是否重新解码、textarea 是否丢 undo/选区以及具体耗时均需运行时验证。

**改法**：
- 首次 `open_preview_window` 时构建一次骨架（panel / img / textarea / footer 节点 + 监听器）；
- `loadTodo` 只做原地更新：`textarea.value`（仅当未聚焦时改，避免抢光标）、`img.src`（相同 url 跳过，避免重解码）、`textContent`、`classList.toggle`；
- 用 `dataset.todoId` 判定是否同一 todo，相同则跳过。

**为什么是 S 级候选**：这是 Usticky 独有的、Musage 完全没有的结构重建，且发生在用户最敏感的"hover 即出、跟手"交互上；实际收益以 MutationObserver 和 p50/p95 为准。

**验证**：`performance.mark('preview-load-start')` / `'preview-load-end'` 包 `loadTodo`；用 MutationObserver 统计每次换卡的 added/removed nodes，并分别测文本、图片、GIF。

---

#### S3 · hover-pos 约 20Hz 调 elementFromPoint，未 rAF 合并

**现状**（[main.ts:1907-1990](file:///Users/wyh/Project/Usticky/src/main.ts#L1907-L1990)）：
- Rust hover emitter 每 50ms 发 `floating-hover-pos`（inside=true 时，最多 20Hz）；
- 前端 listener 直接调 `document.elementFromPoint(x, y)` 命中卡片，无 `requestAnimationFrame` 合并；
- 每 tick 还调 `document.getElementById("app")`（未缓存）；
- 代码注释把它描述为 hit-test 路径；它是否在当前 WKWebView/WebView2 版本的 dirty-layout 场景触发同步布局，不能仅凭这段代码判定，应以 Web Inspector 录制为准。

**量化**：
- 卡片切换边沿会改变 class 和按钮显隐，可能使下一次 hit-test 变贵；是否产生 Layout 长任务需要 profile；
- 当前确定的成本是每 50ms 一次 IPC payload、事件解包、DOM hit-test 和状态机分支；
- 稳态同卡时仍会重复处理事件，因此静止 10 秒约 200 次事件是可直接验证的浪费上限。

**改法**：
- listener 只把 payload 存进一个 `latestHoverPos` 变量；
- 用单个 `requestAnimationFrame` 循环在帧内消费（rAF 内 elementFromPoint 的布局 flush 与本帧其它布局合并，不产生额外布局）；
- 缓存 `appEl`（模块级变量，init 时取一次）；
- 修正注释。

**佐证**：MDN 的 [`requestAnimationFrame`](https://developer.mozilla.org/en-US/docs/Web/API/Window/requestAnimationFrame) 文档支持用 rAF 对齐下一次 repaint；这支持把 rAF 作为实验方案，但不等于证明 `elementFromPoint` 每次都会强制布局。

**为什么是 S 级候选**：hover 是 Usticky 最高频的持续交互，且这是 Musage 不存在的调用路径；实际是否达到 S 级，要由静止/移动 profile 决定。

**验证**：DevTools Performance 面板录制 5s 横扫卡片，同时看事件数量、handler wall time、Recalculate Style/Layout；不要预设改后 Layout 条必须归零。

---

### A 级候选（长列表或输入路径，需基准确认）

#### A1 · 逐卡 10 个 addEventListener → 事件委托

**现状**（[main.ts:648-712](file:///Users/wyh/Project/Usticky/src/main.ts#L648-L712)）：
`buildTodoRow` 对每张卡绑定：
- check / title / copy / delete 各 1 个 click（4）
- copy / delete 各 mouseenter + mouseleave（4）
- 卡片本身 mouseenter + mouseleave（2）

合计 **10 个监听器/卡**。N=100 → 首屏 1000 个监听器。

**Musage 对照边界**：Musage 的 `onAppActionClick` 委托主要处理 `.err-btn` 与 `.empty-state-cta`（[main.ts:1338-1383](file:///Users/wyh/Project/Musage/src/main.ts#L1338-L1383)，在 [main.ts:1437](file:///Users/wyh/Project/Musage/src/main.ts#L1437) 注册），不是所有 provider 卡片动作；不能把它直接描述成 Usticky 卡片交互的同构实现。

**量化**：
- N=100 时静态计数约 1000 个 listener；addEventListener 和闭包的实际构建/内存成本取决于 WebView 版本与闭包捕获，需用 Performance/Memory profile 测量，不采用固定微秒和 MB 估算；
- 更大的隐性成本：`render()` 全量重建时旧监听器随节点 GC，但若有未清理引用会泄漏；diff 复用节点时监听器持续累积风险。

**改法**：
- click 委托到 `.todo-list`（pending/done 各一个或共同父容器），用 `closest('[data-action]')` 分发 check/copy/delete/title；
- 卡片 hover 用 `mouseover`/`mouseout`（冒泡）委托，靠 `relatedTarget` 判定是否真的进出卡片；
- 按钮 hover 反馈同理委托，或用 CSS `.card-hover [data-action]:hover`（但项目规则"浮窗内不准 CSS :hover"——所以仍走 class 委托）。

**注意**：mouseenter/mouseleave 不冒泡，委托必须改用 mouseover/mouseout + relatedTarget 判定，这是改造的主要复杂度。

**验证**：`getEventListeners(document.querySelector('.todo-list'))` 改前显示 N×10 个子节点监听器、改后父容器 3-4 个；首屏 `performance.measure('render')` 对比。

---

#### A2 · updateTodoRow 每行 7 次 querySelector → 缓存引用

**现状**（[main.ts:731-840](file:///Users/wyh/Project/Usticky/src/main.ts#L731-L840)）：
每次 render 对**每一行**调用 updateTodoRow，内部 7 次 `row.querySelector(...)`（check/title/banner/copy/del/actions/...）。

**Musage 对照**：Musage 的 `updateCard`（[main.ts:768-820](file:///Users/wyh/Project/Musage/src/main.ts#L768-L820)）**也是逐行 querySelector**——所以这**不是 Usticky 回归**，是双方共同模式。但 Usticky 列表可能比 Musage 的 ~11 个 provider 长得多，放大效应更明显。

**量化**：N=100 每次 render 静态计数约 700 次 row-level querySelector；缓存 refs 后稳态查询可接近 0。单次查询耗时和总 wall time必须由当前 WebView profile 给出。

**改法**：
- `buildTodoRow` 时把关键子节点引用存进 `WeakMap<HTMLElement, RowRefs>` 或挂到 `row._refs`（非枚举属性）；
- `updateTodoRow` 从缓存取，省掉选择器解析。

**验证**：在 updateTodoRow 循环前后 `performance.mark`，造 100 条 todo，触发一次 render，对比耗时。

---

#### A3 · addTodo 等 fsync 才清空输入框 → 乐观更新

**现状**（[main.ts:2474](file:///Users/wyh/Project/Usticky/src/main.ts#L2474)）：
quick-add 按 Enter 后 `await addTodo(trimmed)`，而 `add_todo` 命令内部 `persist_and_emit` 做 `to_vec_pretty + File create + write + sync_all + rename + dir fsync`（5-30ms，磁盘忙时更高）。**await 完成后才清空 input**，新卡片也要等 emit 到达才出现。

**量化**：Enter 到输入框清空 / 新卡出现 = IPC 往返 + fsync ≈ 5-30ms。"3 秒写下收起"场景下通常不可感知，但磁盘忙或 N 很大（pretty 序列化变慢）时可能逼近 50ms。

**改法**：
- 前端本地生成临时 todo（client-side id）立即插入 DOM + 清空 input；
- 后端 `add_todo` 内存更新后立即返回，persist 放后台 spawn（失败 emit `persist-failed`，前端回滚 + mini-flash）；
- 或保持 await IPC 但把 fsync 从关键路径摘掉（内存写成功即返回，后台持久化）。

**权衡**：这是耐久性 vs 延迟的取舍。本地 todo 工具崩溃概率极低，但掉电可能丢最后一条。建议：内存写立即返回，`sync_all` 仍做但放后台任务（不阻塞命令返回）。失败已有 `persist-failed` 通道。

**验证**：Enter 到 input.value 清空的 `performance.now()` 差值；磁盘压力下（`ping -i 0.2` 造 IO）对比。

---

### B 级（修了能优化但用户未必感知）

#### B1 · 预览窗 18px blur 与浮窗 N 卡 blur 同时活跃
[preview.css:42](file:///Users/wyh/Project/Usticky/src/preview.css#L42) 的 `.preview-panel` 是 `blur(18px)`，预览开启时与浮窗多卡 backdrop-filter 同时存在。WebKit 官方只确认 backdrop filter 会增加 rendering passes；不能从半径/面积静态推导线性 GPU 收益。建议把 10-12px 仅作为 A/B 实验，用 Composite/Paint p95 与视觉验收决定。

#### B2 · setCardHover 在边沿读 offsetWidth
[main.ts:968,976](file:///Users/wyh/Project/Usticky/src/main.ts#L968) 读 `thumb.offsetWidth`（强制布局）后写 `thumb.style.flex`。已被幂等守卫保护（只在 enter/leave 边沿跑一次，非每 tick），影响小。可改 CSS-only 冻结方案或进入时一次性测量缓存。

#### B3 · wheel 每事件读 scrollHeight/clientHeight
[main.ts:1438-1445](file:///Users/wyh/Project/Usticky/src/main.ts#L1438-L1445) 在 `passive:false` 的 wheel 监听里每滚动事件读 `scrollHeight`/`clientHeight`（60-120 次/s）。布局读本身在无写时廉价，但可加 rAF 节流或缓存。Musage 同款，非回归。

#### B4 · pretty JSON + 每次全量 fsync
先用真实 10/100/500 todo StoreData 比较 `to_vec_pretty` 与 compact 的字节数和序列化时间；收益取决于字段内容，不能预设 20-30%。reorder 是否 debounce 也要先看交互频率和 crash durability 要求；add/delete 仍应立即持久化。

#### B5 · get_attachments_dir 启动即 create_dir_all
把 `create_dir_all` 从首屏移到首次 `paste_from_clipboard` 时懒创建。省一次首屏 syscall。

#### B6 · Win 隐藏态 hover emitter 无 gate
[platform/windows.rs](file:///Users/wyh/Project/Usticky/src-tauri/src/platform/windows.rs) 的 emitter 隐藏时仍每 tick 做 `GetCursorPos` + `WindowFromPoint` + `GetAncestor` 等 Win32 调用。macOS 路径已有可见性 gate，Win 端漏了。增加 gate 可跳过隐藏态 hit-test，但线程仍会每 50ms sleep/wake，不能描述成 CPU 归零。

---

### 误判丢弃（看着像瓶颈其实不是）

| 候选 | 为什么不是 |
|---|---|
| **macOS hover emitter 每 50ms `run_on_main_thread` + Condvar** | Musage 的 [platform/macos.rs](file:///Users/wyh/Project/Musage/src-tauri/src/platform/macos.rs) **完全相同**的 50ms 模式。Condvar wait 在后台线程，不阻塞 UI；已有 3-tick 失败容忍。不是 Usticky 回归。 |
| **CSS blur 10px / will-change / 心跳动画** | Usticky 反而比 Musage **轻**（blur 10px vs 28px，无 box-shadow transition 动画）。心跳动画是 compositor-only 的 transform/opacity 0.001° 亚像素微动，正确实现，不在主线程产生每帧工作。不是慢的原因。 |
| **hover tick 50ms 太低/太高** | 已是 20Hz，与 Musage 持平。再低会损响应，再高会增 IPC。不动。 |
| **crate-type / lto / opt-level** | 已按 AGENTS.md 配齐（`panic=abort` + `lto=true` + `opt-level="s"`），无优化空间。 |
| **tray rebuild 频繁** | `rebuild_tray` 只在用户切换 locale/pin-mode/shortcut 时触发，无周期路径；静态审查可排除“持续热点”，单次耗时仍需实测。 |
| **每键击监听** | 代码里**零** per-keystroke 高频监听（无 input 事件、无倒计时 setInterval）。输入响应本身是干净的。这是 Usticky 相对 Musage 的改进点（Musage 有 1s 倒计时轮询）。 |
| **setInterval 轮询** | 前端**零** `setInterval`（已确认）。唯一周期是 Rust hover emitter。 |
| **updateTodoRow 逐行 querySelector 是 Usticky 独有烂代码** | 不是——Musage updateCard 也逐行 querySelector。是共同模式，仅在长列表时放大，降为 A 级而非 S 级回归。 |

---

## 快速验证清单

1. **冷启动**：`init()` 首行 `performance.mark('boot-start')` → `render()` 后 `mark('boot-first-render')` → `measure`。改前改后各测至少 20 次，报告 p50/p95，不预设固定毫秒收益。
2. **预览换卡**：`loadTodo` 前后 mark + MutationObserver 数 addedNodes。目标：改后稳定骨架不再整棵重建；实际耗时以 p50/p95 为准。
3. **hover 平滑度**：DevTools Performance 录制 5s 横扫卡片，统计事件数、handler wall time 与相邻 Layout 条目；先证明瓶颈属于 IPC、JS 还是布局。
4. **首屏监听器数**：控制台 `document.querySelectorAll('.todo-card').length` × 10 vs 改后父容器委托数。
5. **长列表 render**：造 100 条 todo，`performance.mark` 包 `render()`，对比 A2 改前改后。
6. **Enter 延迟**：keydown Enter 到 `input.value===''` 的 `performance.now()` 差，对比 A3 改前改后（含磁盘压力测试）。
7. **GPU（预览开启）**：DevTools Performance 看 "Composite Layers" 时长，B1 blur 降半径前后对比。
8. **对照 Musage 基线**：同机同录 DevTools Performance，冷启 + hover 5s，对比主线程 flame chart 的 Scripting/Rendering/Painting 占比。

---

## 非 perf 发现（安全 / 稳定性，不展开）

本轮没有发现能够仅凭当前源码直接证明的安全或数据丢失问题。`run_on_main_thread` 内的 `NSWindow` 裸指针生命周期值得单独做 objc2/AppKit 生命周期审查，但当前证据不足以断言 use-after-free，因此不列为已确认 finding。

---

## 建议落地顺序

1. **先测 S1 + S2 + S3**：冷启动、预览换卡、hover 三组 p50/p95，确认真实排序。
2. **实测命中后再做 S1/S3/B5/B6**（低风险候选），每项单独 A/B，避免收益混淆。
3. **再做 S2**（预览窗骨架复用重构），验证图片/GIF、编辑、删除、固定窗全路径。
4. **A1 事件委托**只在 100/500 卡首次 render 显示 listener 成本占比明显时排期。
5. **A2 缓存引用**按 7N querySelector profile 决定。
6. **A3 乐观更新**需要先明确耐久性与失败回滚约束。
7. B 级按需穿插。

Stage 4（baseline 测量脚本）待本报告确认后再写。

## 官方资料

- [Tauri · Inter-Process Communication](https://v2.tauri.app/concept/inter-process-communication/)：commands/events 跨 WebView 与 Core，采用异步消息传递；支持把 20Hz hover event 视为需要测量的 IPC 路径。
- [Tauri · Calling Rust from the Frontend](https://v2.tauri.app/develop/calling-rust/)：async commands 的执行模型；支持区分命令响应延迟与 UI 主线程阻塞。
- [Tauri · Calling the Frontend from Rust](https://v2.tauri.app/develop/calling-frontend/)：事件 payload、listen/unlisten 与快速连续事件的行为。
- [WebKit · Introducing Backdrop Filters](https://webkit.org/blog/3632/introducing-backdrop-filters/)：backdrop filter 的 blur/composite 流程及其增加 rendering passes 的官方警告。
- [MDN · will-change](https://developer.mozilla.org/en-US/docs/Web/CSS/will-change)：过度使用 `will-change` 可能消耗额外资源，应通过实测决定是否保留。
- [MDN · requestAnimationFrame](https://developer.mozilla.org/en-US/docs/Web/API/Window/requestAnimationFrame)：在下一次 repaint 前安排更新的语义；支持作为 hover 事件合并实验，不构成 forced-layout 证明。
- [Apple · WKProcessPool](https://developer.apple.com/documentation/webkit/wkprocesspool)：多个 WKWebView 的进程隔离/共享背景；动态预览 webview 的冷启成本应实测。
- [objc2-app-kit · NSWindow](https://docs.rs/objc2-app-kit/latest/objc2_app_kit/struct.NSWindow.html)：当前 AppKit/objc2 API 参考；文档没有为 `setLevel`/`windowNumberAtPoint` 提供固定性能承诺。

---

## 2026-08-11 校正附录

本附录基于当前工作树重新核对，用来约束上文中未经实机 profile 的强断言；不改变候选项的优先级，只修正证据等级和对照边界。

### 1. 预览窗全量 render：候选成立，但收益数字未证实

`src/preview.ts:89-187` 确实在每次 `loadTodo()` 时清空 `appEl`，重新创建 panel、textarea、footer、按钮和监听器；所以 S2 是当前源码可以直接证明的结构性开销。可是“每秒 10 张卡会产生 100-200 次节点创建”“图片每次重新解码 1-5ms”“DOM 工作减少 80-90%”都必须通过实际 profile 或 MutationObserver/Performance API 取得，不能从代码行数直接推出。报告中的 `textarea` 丢 undo/selection 也只有在外部事件落到正在编辑的 textarea 时才成立，不能当成每次换卡必发的行为。

### 2. `elementFromPoint`：保留为高频 IPC 候选，撤销“必然 forced layout”结论

当前路径的确定事实是：macOS 和 Windows 在 inside 状态下约每 50ms 发一次 `floating-hover-pos`，前端每次执行 `elementFromPoint`、`closest`，卡片切换时再读 DOM；这是 Usticky 相比 Musage 多出的持续工作。当前代码注释把它描述为 hit-test，静态资料不足以证明它在每个 tick 都强制同步布局；“layout dirty 时必 flush”“改后离帧 Layout 归零”应降为待验证假设。

建议验证顺序：先统计静止/移动时事件数量和 listener wall time，再看 Web Inspector 的 Recalculate Style/Layout 是否实际与 handler 相邻。只有 profile 证明存在布局长任务，才把 rAF 合并列为修复；否则优先做坐标/命中结果去重，静止 10 秒的事件上限约为 200 次。

### 3. Musage 的事件委托对比需要收窄

Musage `src/main.ts:1338-1383` 的 `onAppActionClick` + `src/main.ts:1437` 委托的是错误卡片/空态 CTA（`.err-btn`、`.empty-state-cta`），不是所有 provider 卡片动作。不能据此写成“ Musage 对全部卡片只用一个父容器 click，Usticky 需要从 10N 追平到 3 个”。Usticky 的每卡 click/mouseenter/leave 数量仍然是真实的首次构建成本，但事件委托方案是新的设计选择，不是已证实的 Musage 同构实现；应以 100/500 卡首次 render profile 决定是否值得改造。

### 4. CSS 玻璃结论应以当前变量为准

当前 `src/styles.css:142-145` 的注释和卡片规则使用约 10px 的浮窗 blur，Musage provider 卡片仍是约 28px；这一点支持“Usticky 不是因为浮窗 blur 半径更大而慢”。但 `.todo-card` 仍逐卡使用 `backdrop-filter`、`will-change`、heartbeat，不能因此完全排除卡片数量带来的合成/绘制规模效应。正确表述是“半径更轻，但实例数更多且无 todo 数量上限”，需在 50/100 卡下测长帧，不应把它写成固定 GPU layer 数或固定倍数。

### 5. 其他收益数字的证据等级

- “串行 invoke -3~8ms”“Enter -5~30ms”“querySelector -60~70%”“省 0.5MB”应视为测量目标，不是静态审查结论；冷启动和输入延迟必须用 p50/p95 实机采样。
- `get_attachments_dir` 的 `create_dir_all` 确实发生在 `src-tauri/src/commands/mod.rs:706-712`，懒创建可省一次启动文件系统调用，但不能预报具体 1-2ms。
- Windows emitter 当前循环没有可见性 gate；增加 gate 的目标是隐藏窗口时跳过 `GetCursorPos`/`WindowFromPoint`/`GetAncestor` 路径，但“CPU 归零”不准确，线程仍会按 50ms sleep 唤醒，且 gate 本身也需要可靠的可见性状态同步。
- `to_vec_pretty` 的体积/速度收益与数据内容、格式化字符串和存储介质相关；不要把“约 30%/20-30%”当作普适结果。父目录 fsync 是 durability 约束，必须保留到有明确权衡和基准为止。

### 6. 校正后的首要验证顺序

1. 冷启动：记录五个 startup invoke 的单段与串行总时长，确认 S1 是否达到用户可感知的 p95。
2. 预览换卡：记录 `render()` 的节点创建数、图片 load、textarea focus 状态和 handler wall time，确认 S2 的实际尾延迟。
3. hover：静止 10 秒统计事件数；横扫 20 卡录制 JS/Layout/paint，先证明成本属于 IPC、JS 还是布局。
4. Windows：隐藏/显示窗口各录 10 秒 emitter counters，确认 gate 的真实省时，再决定是否落地。
