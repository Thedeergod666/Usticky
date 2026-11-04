# 浮窗玻璃 A/B 验收手册（v0.2.10 窗口级哨兵）

> **目标**：在 `pnpm tauri dev` 跑起来的浮窗上，用 devtools console 一两行命令，10 秒内判定
> v0.2.10 的**核心未验证假设**：
>
> > macOS WKWebView 对非 key / 透明窗口的 **backdrop 采样节流是「窗口级」判定**（整窗一起判定空闲 → 一起丢采样），
> > 而不是每个元素独立判定。若是窗口级，1 个持续动画的哨兵（`.glass-keepalive`）就能保住 N 张卡片的采样；
> > 若是元素级，玻璃仍会在 ~2s 后消失。
>
> 本手册只做**判定**，不改任何源文件。所有结论都写进文末的**记录表**。

---

## 0. 速查（30 秒版）

| # | 判定项 | 一句话命令 | 期望 |
|---|---|---|---|
| 0 | 环境自检 | `A` | `pinMode: 'pin_top'`、`lowPower: 'off'`、`hasGetAnimations: true` |
| 1 | 哨兵活着 | `B` + `C` | `cssAnimationNames: ['usticky-backdrop-heartbeat']`、`verdict: 'ALIVE'` |
| 2 | 卡片零 animation | `D` | `elementsWithAnimation: []`、`allCssAnimationNames: ['usticky-backdrop-heartbeat']` |
| 3 | **核心 A/B** | `E1`（移除）→ `E2`（恢复） | 移除后 ~2s 玻璃掉 = **假设成立**；不掉 = **假设不成立** |
| 4 | 归位滑动 | `F1`（布防）→ 拖拽 → `F2`（读数） | `distinctTranslations >= 3` **且** `amplitudeOK === true`（位移幅值 ≈ 一个行距）= 滑动（通过）。只看"有几个非零值"会漏判被截断的情形（见 §5.3 失败签名 B） |
| 5 | 逐卡玻璃 + 卡缝 | `G` | `elementsWithBackdropFilterAtGap: []` = 卡缝是真空隙 |
| 6 | done 保留区 | `H` | `verdict: 'PASS: done reserved area has NO backdrop-filter…'` |

> 文中的 `A` / `B` / `C` … 是**下文各节标题下的代码块**，直接整段复制粘贴进 console 即可。
> 每段都是自包含的 IIFE，**不会污染全局**（唯一例外：`F1` 会写 `window.__probe`，这是有意的）。

---

## 1. 前置准备（不做这步，实验结论不可信）

### 1.1 必须做的三件事

| # | 事项 | 怎么做 | 为什么 |
|---|---|---|---|
| P1 | **切到 PinTop 模式** | 托盘图标右键 → Settings 子菜单 → `Pin Top`；或设置面板里切 | ① `PinBottom` 时鼠标移入会**抬升 NSWindow level** → 触发 L3 `usticky://backdrop-refresh` → **强制重采样玻璃**，会把"哨兵失效"的结果洗掉（假阴性）；② `PinBottom` 的窗口在所有普通窗口**之下**，被别的窗口挡住时 WKWebView 会**暂停合成 / 节流 rAF**，静止实验直接做不成 |
| P2 | **devtools 改成独立窗口** | DevTools 右上角 `⋮` → Dock side → 选 **Undock into separate window**，并把它摆在浮窗**旁边**（不要盖住浮窗） | DevTools 默认 **docked 在 Tauri 窗口里**。鼠标一移到 console，浮窗就失焦 / 失 hover → 玻璃强弱会随鼠标进出变化，实验读不出来 |
| P3 | **把浮窗放到有纹理的背景上** | 拖到一张照片 / 一屏文字密集的窗口上面 | blur 在 vs. blur 没了，在**纯色**背景上几乎看不出差别；必须靠纹理判断 |

### 1.2 确认没有干扰

- **低电量模式必须关**：`body[data-low-power]` 存在时，CSS 里有
  `body[data-low-power] * { animation: none !important; backdrop-filter: none !important; }`
  —— 哨兵的动画会被一起关掉，A/B 实验直接失去意义。
  （当前版本这个属性**还没有接线**，正常不会自动出现；但 `A` 段会顺手确认。）
- **别在实验期间碰设置面板 / 预览窗**：预览窗开 / 关会 emit `usticky://backdrop-refresh`（L3）→ 重采样玻璃。
- **别滚动列表**：滚动会改合成层，也会被 `elementsFromPoint` 的坐标假设干扰。

### 1.3 环境自检 —— `A`

```js
// A: environment preflight
(() => {
  const b = document.body;
  return {
    pinMode: b.dataset.pinMode ?? '(unset)',
    hover: b.dataset.hover ?? '(off)',
    lowPower: b.dataset.lowPower ? 'ON  <-- 关掉它，哨兵动画会被 animation:none 掐掉' : 'off',
    narrowTier: b.dataset.narrow === undefined ? '(unset)' : (b.dataset.narrow === '' ? '(normal width)' : 'narrow tier ' + b.dataset.narrow),
    hasGetAnimations: typeof document.getAnimations === 'function',
    reducedMotion: matchMedia('(prefers-reduced-motion: reduce)').matches,
    sentinelPresent: !!document.querySelector('.glass-keepalive'),
    pendingCards: document.querySelectorAll('.todo-section-pending .todo-card').length,
    doneCards: document.querySelectorAll('.todo-section-done .todo-card').length,
    webkit: navigator.userAgent.includes('AppleWebKit') ? 'AppleWebKit' : navigator.userAgent,
  };
})()
```

**预期输出**

```text
{
  pinMode: 'pin_top',        // ← 必须是 pin_top（见 P1）
  hover: '(off)',            // ← 跑 A 时鼠标最好在浮窗外
  lowPower: 'off',           // ← 必须是 off
  narrowTier: '(normal)',
  hasGetAnimations: true,    // ← WKWebView Safari 13.1+ 才有；false 时走 §2 的 fallback
  reducedMotion: false,      // ← true 的话 WebKit 可能压掉动画，见 §9 坑 3
  sentinelPresent: true,
  pendingCards: 5,           // ← §4/§5 需要 >= 3 张 pending 卡
  doneCards: 2,              // ← §6 需要 >= 1 张 done 卡（否则没有保留区可测）
  webkit: 'AppleWebKit/...'
}
```

> `pendingCards` 不足 3 张 → 先在输入框里加几条再继续（§4 的探针要挑"第 3 张"当被观察对象）。
> `doneCards` 为 0 → 先勾掉一条（§6 才能测）。

---

## 2. 判定项 1：哨兵是否活着

### 2.1 静态快照 —— `B`

```js
// B: sentinel static snapshot
(() => {
  const s = document.querySelector('.glass-keepalive');
  if (!s) return 'FAIL: .glass-keepalive not in DOM';
  const cs = getComputedStyle(s);
  const hasGA = typeof document.getAnimations === 'function';
  const ga = hasGA ? document.getAnimations() : [];
  const cssAnims = ga.filter(a => !!a.animationName);            // CSSTransition 没有 animationName
  const mine = cssAnims.filter(a => (a.effect && a.effect.target) === s);
  return {
    hasGetAnimations: hasGA,
    getAnimationsTotal: ga.length,          // 可能 > 1（含正在跑的 transition），别当断言
    cssAnimationNames: [...new Set(cssAnims.map(a => a.animationName))],
    heartbeatCount: mine.length,            // ← 断言：恰好 1
    heartbeatPlayState: mine.map(a => a.playState),
    computedAnimationName: cs.animationName,
    computedDuration: cs.animationDuration,
    computedIteration: cs.animationIterationCount,
    computedTiming: cs.animationTimingFunction,
    computedPlayState: cs.animationPlayState,
    computedBackdropFilter: cs.backdropFilter || cs.webkitBackdropFilter || 'none',
    computedWillChange: cs.willChange,
    computedOpacity: cs.opacity,
    computedZIndex: cs.zIndex,
    computedPointerEvents: cs.pointerEvents,
    rect: (r => ({ x: Math.round(r.x), y: Math.round(r.y), w: Math.round(r.width), h: Math.round(r.height) }))(s.getBoundingClientRect()),
  };
})()
```

**预期输出**

```text
{
  hasGetAnimations: true,
  getAnimationsTotal: 1,                 // 恰好 1 最理想；>1 也行（见 §9 坑 12）
  cssAnimationNames: ['usticky-backdrop-heartbeat'],   // ← 断言：只有这一个 CSS 动画
  heartbeatCount: 1,                     // ← 断言：恰好 1
  heartbeatPlayState: ['running'],
  computedAnimationName: 'usticky-backdrop-heartbeat',
  computedDuration: '4s',
  computedIteration: 'infinite',
  computedTiming: 'linear',
  computedPlayState: 'running',
  computedBackdropFilter: 'blur(10px) saturate(1.8)',  // ← 注意是 1.8 不是 180%（§9 坑 5）
  computedWillChange: 'transform',
  computedOpacity: '0.001',          // ← 动画中会在 0.001 ~ 0.002 之间抖（keyframes 声明了 opacity）
  computedZIndex: '-1',
  computedPointerEvents: 'none',
  rect: { x: 0, y: 0, w: 1, h: 1 }
}
```

**断言清单**
- `cssAnimationNames` 的**唯一元素**是 `usticky-backdrop-heartbeat` → 全窗口只有哨兵在跑动画。
- `heartbeatCount === 1` → 不是 0（动画没起来）也不是 2+（有第二个心跳源）。
- `computedBackdropFilter !== 'none'` → 哨兵确实是一个**真的** backdrop-filter 元素。
  （如果这里是 `none`，说明 `var(--tile-blur)` 继承问题在这里发生过 —— 不过哨兵写的是硬编码值，
  正常不会；写成 `none` 就等于哨兵只是个普通动画元素，机制不成立。）

### 2.2 运行时活性 —— `C`（**这一段才是"活着"的证明**）

`getComputedStyle().animationName` 只能证明"**声明了**动画"，不能证明"**在跑**"。
`C` 用 `Animation.currentTime` 推进 + computed `transform` 变化双重确认。

```js
// C: sentinel liveness (runtime) — paste and wait ~0.6s
(async () => {
  const s = document.querySelector('.glass-keepalive');
  if (!s) return 'FAIL: no sentinel';
  const num = v => (v && typeof v === 'object' && 'value' in v) ? Number(v.value) : Number(v);
  const hasGA = typeof document.getAnimations === 'function';
  const ga = hasGA ? document.getAnimations() : [];
  const a = ga.find(x => x.animationName === 'usticky-backdrop-heartbeat');
  const t0 = a ? num(a.currentTime) : null;
  const tf0 = getComputedStyle(s).transform;
  const op0 = getComputedStyle(s).opacity;
  await new Promise(r => setTimeout(r, 600));
  const t1 = a ? num(a.currentTime) : null;
  const tf1 = getComputedStyle(s).transform;
  const op1 = getComputedStyle(s).opacity;
  const advanced = t0 !== null && t1 !== null && t1 > t0;
  const changed = tf0 !== tf1 || op0 !== op1;
  return {
    hasGetAnimations: hasGA,
    playState: a ? a.playState : '(animation object not found)',
    currentTimeBefore: t0,
    currentTimeAfter: t1,
    currentTimeAdvanced: advanced,
    transformBefore: tf0, transformAfter: tf1,
    opacityBefore: op0,    opacityAfter: op1,
    computedChanged: changed,
    verdict: (advanced || changed) ? 'ALIVE — 哨兵动画在推进' : 'DEAD — 动画没有推进（检查 reduced-motion / 窗口被遮挡 / 元素不可见）',
  };
})()
```

**预期输出**

```text
{
  hasGetAnimations: true,
  playState: 'running',
  currentTimeBefore: 12345.6,
  currentTimeAfter: 12945.6,      // ← 差值 ≈ 600ms，验证 currentTime 真的在走
  currentTimeAdvanced: true,
  transformBefore: 'matrix(1, 0, 0, 1, 0, 0)',
  transformAfter:  'matrix(0.9999999999998477, 0.000017453292519943295, -0.000017453292519943295, 0.9999999999998477, 0, 0)',
  opacityBefore: '0.001', opacityAfter: '0.0011',
  computedChanged: true,
  verdict: 'ALIVE — 哨兵动画在推进'
}
```

> `transform` 的具体数字不重要（0.001° 的亚像素旋转），**只要 before ≠ after** 就行。
> `opacity` 可能是 `0.001` → `0.0011`（也可能因为 WebKit 的精度截断两者相同）—— 所以以 `transform` 为准。

### 2.3 fallback（`getAnimations` 不存在时）

如果 `A` 段返回 `hasGetAnimations: false`（理论上只有 macOS 10.15.0/10.15.1 的老 WKWebView 会这样），
用下面这段代替 `B` / `C`：

```js
// C-fallback: liveness without getAnimations
(async () => {
  const s = document.querySelector('.glass-keepalive');
  if (!s) return 'FAIL: no sentinel';
  const snap = () => {
    const cs = getComputedStyle(s);
    return { name: cs.animationName, dur: cs.animationDuration, iter: cs.animationIterationCount,
             state: cs.animationPlayState, tf: cs.transform, op: cs.opacity };
  };
  const a = snap();
  await new Promise(r => setTimeout(r, 600));
  const b = snap();
  return {
    before: a, after: b,
    declared: a.name !== 'none',
    running: a.state === 'running',
    advanced: a.tf !== b.tf || a.op !== b.op,
    verdict: (a.name !== 'none' && a.state === 'running' && (a.tf !== b.tf || a.op !== b.op))
      ? 'ALIVE (fallback 路径)' : 'DEAD / 无法确认',
  };
})()
```

**预期输出**：`declared: true`、`running: true`、`advanced: true`、`verdict: 'ALIVE (fallback 路径)'`。

---

## 3. 判定项 2：卡片上确实没有任何 animation

这条直接验证 v0.2.10 的前提——"CSS animation 不再锁死卡片的 transform"。
（AGENTS.md v0.2.8 经验 ⑦：运行中的 CSS animation 对它声明的属性优先级**高于一切普通声明，包括 inline style**，
且动画期间该属性的 computed value 被锁死 → SortableJS 的 inline transform 归位滑动被完全掩蔽。）

### `D`

```js
// D: tiles must carry ZERO animation
(() => {
  const tiles = [
    ...document.querySelectorAll('.todo-card'),
    ...document.querySelectorAll('.todo-input'),
    ...document.querySelectorAll('.todo-paste-btn'),
  ];
  const nameOf = el => getComputedStyle(el).animationName;
  const offenders = tiles.filter(el => nameOf(el) !== 'none')
                         .map(el => ({ cls: el.className, animationName: nameOf(el) }));
  const hasGA = typeof document.getAnimations === 'function';
  const ga = hasGA ? document.getAnimations() : [];
  const tileAnims = ga.filter(a => {
    const t = a.effect && a.effect.target;
    return t && t.nodeType === 1 && typeof t.closest === 'function'
        && t.closest('.todo-card, .todo-input, .todo-paste-btn');
  });
  const card = document.querySelector('.todo-card');
  const appEl = document.getElementById('app');
  return {
    tilesChecked: tiles.length,
    tilesWithAnimation: offenders,                       // ← 断言：[]
    animationsTargetingTiles: tileAnims.map(a => a.animationName ?? '(transition)'),   // ← 断言：[]
    allCssAnimationNames: [...new Set(ga.filter(a => a.animationName).map(a => a.animationName))],
    sampleCardTransition: card ? getComputedStyle(card).transition : '(no card)',
    sampleCardWillChange: card ? getComputedStyle(card).willChange : '(no card)',
    dragActiveFlag: appEl?.dataset.dragActive ?? '(absent — 未在拖拽，正常)',
  };
})()
```

**预期输出**

```text
{
  tilesChecked: 7,                 // N 张卡 + 1 个输入瓦片 + 1 个粘贴键
  tilesWithAnimation: [],          // ← 断言
  animationsTargetingTiles: [],    // ← 断言
  allCssAnimationNames: ['usticky-backdrop-heartbeat'],   // ← 断言：全局只有哨兵一个 CSS 动画
  sampleCardTransition: 'opacity 0.22s cubic-bezier(0.22, 1, 0.36, 1)',
  sampleCardWillChange: 'backdrop-filter',
  dragActiveFlag: '(absent — 未在拖拽，正常)'
}
```

**断言清单**
- `tilesWithAnimation` 为 `[]` → 卡片 / 输入瓦片 / 粘贴键上**零** animation。
- `allCssAnimationNames` 只有 `usticky-backdrop-heartbeat` → 全局唯一动画源是哨兵。
- `sampleCardTransition` **不含 `transform`** → 这是 **v0.2.10b 的硬性不变量**。
  样式表里只要留着 transform 过渡，SortableJS 的 FLIP 就失效：`animate()` 第 1 步
  `css(target,'transition','')` **只清 inline**，清不掉样式表的 → 第 3 步"瞬时摆回
  旧视觉位置"变成一段过渡 → 偏移从未落地 → 归位仍是瞬移。transform 过渡必须由
  SortableJS 自己的 inline `transition: transform 150ms` 独占。
  （旧版本这条断言写的是"仍然包含 transform → 过渡还在 → 是滑动的前提"，**推理
  是反的**：SortableJS 自带 inline transition，样式表那条不是前提，是干扰源。）
- `dragActiveFlag` 是 `(absent …)` → 空闲时没有拖拽标记。（v0.2.10b 起这个属性的
  语义从"暂停卡片心跳"变成"**真在拖**"——只有真拖起来才给选中的卡加淡化，避免
  普通单击标题时出现"淡一下又淡回来"的闪烁。它空闲时本就该是 absent。）

> `animationsTargetingTiles` 出现 `'(transition)'` 是**正常且无害**的：SortableJS 归位时会临时给卡片
> 加 inline `transition: transform 150ms`，这是一次**过渡**，不是 animation，不锁死 computed value。
> 断言要看 `tilesWithAnimation`（animationName 是否为 `none`）。

---

## 4. 判定项 3（核心）：A/B 对照实验

### 4.1 实验协议

```
T0  鼠标停在浮窗上（hover 生效 → 玻璃满强度），别动
T1  把鼠标移到 devtools 窗口 → 执行 E1（移除哨兵）
T2  ★ 立刻把鼠标移回浮窗，静止不动 ★   ← 计时起点从这里开始
T3  等 6 秒（> 2s 节流窗口 × 3 倍余量）
T4  判读：玻璃还在吗？
```

> **为什么 T2 才是计时起点**：执行 `E1` 会改 DOM → 触发一次 invalidation → 玻璃会被**重新采样**一次，
> 所以"移除瞬间"看起来一定是对的。如果在 PinBottom 模式下，鼠标移回浮窗还会抬升窗口 level →
> L3 `usticky://backdrop-refresh` → `force-reflow` → **又重采样一次**。
> 这两个都会给你 2 秒的假象。**必须从"鼠标移回浮窗之后"开始计时。**
> （用 PinTop 模式的话，level 不会变，L3 不触发，干扰只剩 DOM invalidation 那一次。）

### 4.2 三个命令

```js
// E1: REMOVE the sentinel  →  让窗口进入"完全静止"状态
(() => {
  const s = document.querySelector('.glass-keepalive');
  if (!s) return 'sentinel already absent';
  s.remove();
  return 'sentinel REMOVED at ' + new Date().toLocaleTimeString()
       + ' — 把鼠标移回浮窗，静止 6 秒后判读';
})()
```

```js
// E2: RESTORE the sentinel without reloading (same page session)
(() => {
  if (document.querySelector('.glass-keepalive')) return 'sentinel already present';
  const s = document.createElement('div');
  s.className = 'glass-keepalive';
  s.setAttribute('aria-hidden', 'true');
  document.body.appendChild(s);
  return 'sentinel RESTORED at ' + new Date().toLocaleTimeString()
       + ' — 把鼠标移回浮窗，观察玻璃是否立刻回来';
})()
```

```js
// E2b: RESTORE by full reload (cleanest — 回到 index.html 的原始状态)
location.reload()
```

> `E2`（不刷新）重建的元素带的是 `.glass-keepalive` class，`styles.css` 里该 class 自带全部样式
> （`position:fixed` / 1px / `opacity:.001` / `backdrop-filter` / `animation`），所以**行为与原始节点完全一致**。
> `main.ts` 从不重写 `<body>` 的子节点（只操作 `#app`），所以删/加哨兵不会与前端逻辑打架。
> 首选 `E2b`（最干净，排除任何残留状态）；`E2` 用于"想在同一页里来回切"的场景。

### 4.3 阳性对照（positive control）—— `E3`

判读"玻璃掉了"之前，先用这段确认**掉的确实是 backdrop 采样**（而不是别的视觉问题）：

```js
// E3: positive control — force a backdrop re-sample (L3 mechanism, done manually)
(() => {
  const els = [...document.querySelectorAll('.todo-card, .todo-input, .todo-paste-btn')];
  els.forEach(el => el.classList.add('force-reflow'));
  setTimeout(() => {
    document.querySelectorAll('.force-reflow').forEach(el => el.classList.remove('force-reflow'));
  }, 100);
  return 'force-reflow 已施加到 ' + els.length + ' 个瓦片（100ms 后自动撤）'
       + ' — 如果玻璃立刻回来，说明掉的确实是 backdrop 采样（L3 机制仍然有效）';
})()
```

`.force-reflow` 会把 `backdrop-filter` 从 `blur(10px)` 改成 `blur(11px)`（`styles.css` L3 段）→
WebKit 判定 backdrop-filter 变了 → **强制重新采样** → 玻璃立刻恢复。

### 4.4 判读矩阵

| 现象 | 结论 | 含义 |
|---|---|---|
| **结果 A** | 移除哨兵后，玻璃**在 ~2s 后消失**（变平 / 失去磨砂感）；`E3` 一按玻璃**立刻回来**；恢复哨兵后玻璃**立刻回来且不再掉** | ✅ **假设成立** —— 节流确实是**窗口级**判定，1 个哨兵就能保住 N 张卡片的采样。v0.2.10 方案成立，可进入用户视觉验收 |
| **结果 B** | 移除哨兵后，玻璃**6 秒后依然在**（不掉了） | ❌ **假设不成立 / 假设错** —— 当前配置下压根没触发节流（可能是 PinTop + 可见窗口时 WebKit 不节流；或 L1 `will-change` 已经足够；或节流另有原因）。**不要急着上备选方案**，先看 §4.5 的补充排查 |
| **结果 B'** | 玻璃掉了，但 `E3` 按下去**玻璃不回来** | ⚠️ 不是采样被丢弃，而是别的原因（瓦片丢了 `backdrop-filter` / 被别的层盖住 / 低电量模式）。查 `D` 段的 `tilesWithAnimation` 和 `G` 段的 `cardBackdropFilters` |
| **结果 C** | 移除哨兵后玻璃掉，但**恢复哨兵也不回来**（要刷新才好） | 采样丢失状态是"粘"的 → 哨兵不是充分条件 → 走 §7 备选方案 ① |

### 4.5 结果 B 的补充排查（先做这三条，再考虑改代码）

在断定"节流是元素级"之前，先排除这三种"根本没节流"的解释：

1. **WebKit 根本没节流**：把 pin mode 切到 **Normal** 或 **PinTop**，让浮窗**完全可见不被遮挡**，
   重跑一次 A/B。如果三种模式都不掉，说明这套 WKWebView 版本 / 这个窗口配置下**不存在**节流，
   那么 L2（心跳）整层就是**多余的**——可以进一步简化：把 `.glass-keepalive` 的 `backdrop-filter` 去掉
   （只留一个 1px 的纯动画元素），成本更低；甚至连哨兵都可以删。
2. **L1 已经足够**：`will-change: backdrop-filter`（`styles.css` L1 段）给每个瓦片独立 GPU layer，
   **不进层级空闲节流判定**。有可能 L1 单独就够了，L2 是冗余。
   验证：在 console 里临时给所有瓦片加 `will-change`（已经是了）→ 无关。
   换个角度：把哨兵的 `animation` 停掉但**保留元素**（`s.style.animation = 'none'`）→
   如果玻璃不掉，说明心跳本身没用（只是元素存在就够了）。
3. **2s 窗口的判据没对上**：用户最初报的是"整窗玻璃同时消失"，那可能是 **PinBottom + 遮挡**
   场景下的另一个 bug（窗口被完全遮挡 → WKWebView 丢弃合成层），**不是 backdrop 节流**。
   在 PinTop + 可见窗口下复现不出来 → 假设的前提就不成立。

> 这三条排查**都不需要改任何源文件**，全部是 console 片段：
>
> **B-1a** —— 停掉心跳动画，但**保留元素**（测"到底是动画在起作用，还是元素存在就够了"）：
>
> ```js
> // B-1a: stop the heartbeat but keep the element
> (() => { const s = document.querySelector('.glass-keepalive'); if (!s) return 'no sentinel'; s.style.animation = 'none'; return 'heartbeat STOPPED (element still present) — 静止 6 秒观察'; })()
> ```
>
> **B-1b** —— 恢复心跳：
>
> ```js
> // B-1b: resume the heartbeat
> (() => { const s = document.querySelector('.glass-keepalive'); if (!s) return 'no sentinel'; s.style.animation = ''; return 'heartbeat RESUMED'; })()
> ```

---

## 5. 判定项 4：SortableJS 归位是否被掩蔽

### 5.1 判据

SortableJS 1.15.7 的 `animate()`（`node_modules/sortablejs/modular/sortable.esm.js:643-666`，**逐行核对过源码**）在归位时做五步 —— 这是经典 FLIP：

```js
css(target, 'transition', '');                                    // 1. 清 inline transition
css(target, 'transform', '');                                     // 2. 清 inline transform
css(target, 'transform', 'translate3d(dx,dy,0)');                  // 3. 瞬时摆回旧视觉位置
this.forRepaintDummy = repaint(target);                            //    强制重排
css(target, 'transition', 'transform 150ms');                      // 4. 开 inline 过渡
css(target, 'transform', 'translate3d(0,0,0)');                   // 5. 目标位置 → 触发过渡
```

**第 3 步必须"瞬时"生效**，第 5 步的过渡才有东西可滑。三个坑：

- **v0.2.8（逐卡心跳）**：卡片上 `animation: usticky-backdrop-heartbeat`，keyframes **声明了 `transform`**
  → running animation 覆盖 inline style、锁死 computed transform
  → 第 3 步被完全忽略 → 卡片直接落回新布局位置 → **瞬移**（用户感受的"卡顿感"）。
- **v0.2.10（只删心跳）**：卡片零 animation → 第 3 步生效了，但**样式表**的
  `transition: transform 0.18s` 接管了第 1 步清掉的 inline transition → 第 3 步
  从"瞬时摆位"变成一段 180ms 过渡 → FLIP 偏移从未真正落地 → **仍然是瞬移**。
  ⚠️ 第 1 步只清 **inline** transition，**清不掉样式表的** —— 这是最容易漏的一环。
- **v0.2.10b（当前）**：`.todo-card` 的 transition **只剩 opacity**，transform 过渡
  由 SortableJS 的 inline `transition: transform 150ms` 独占 → 第 3 步瞬时、
  第 5 步过渡 → **滑动归位**。

**所以判据是：归位过程中，被观察卡的 `getComputedStyle(el).transform` 必须出现"非零平移"的中间值。**

> ⚠️ **必须读 computed value，不能读 `el.style.transform`**。
> `el.style.transform` 是 inline 的**目标值**（瞬间从 `translate3d(dx,dy,0)` 跳到 `translate3d(0,0,0)`），
> 在新旧两种实现下**看起来一样**。中间过程只存在于 computed value 里。

> ⚠️ **只看"有没有非零平移"不够**（v0.2.10b 补）：若 FLIP 偏移被截断，采样仍会
> 出现多个不同的非零值、只是幅值很小 → 会被误判成"滑动"。必须同时看**幅值**——
> 见 §5.4 的 `maxAbsTranslation` 判据。

### 5.2 布防 —— `F1`（**先跑这段，再去拖**）

```js
// F1: arm the drag probe (run this FIRST, then drag with the mouse)
(() => {
  const cards = [...document.querySelectorAll('.todo-section-pending .todo-card')];
  if (cards.length < 3) return 'NEED >= 3 pending cards (add some first)';
  const target = cards[2];                                  // 观察第 3 张（被拖动时会移位）
  const parse = m => {
    if (!m || m === 'none') return null;
    const n = (m.match(/-?\d*\.?\d+(?:e[-+]?\d+)?/gi) || []).map(Number);
    if (n.length < 6 || n.some(v => !Number.isFinite(v))) return null;
    return m.startsWith('matrix3d') ? { tx: n[12], ty: n[13] } : { tx: n[4], ty: n[5] };
  };
  const probe = { id: target.dataset.todoId, samples: [], stop: false };
  window.__probe = probe;
  const loop = () => {
    if (probe.stop) return;
    probe.samples.push({ t: Math.round(performance.now()), tf: parse(getComputedStyle(target).transform) });
    requestAnimationFrame(loop);
  };
  requestAnimationFrame(loop);
  return 'ARMED on pending card #' + probe.id + ' (第 3 张)'
       + ' — 现在用鼠标把第 1 张 pending 卡往下拖过第 3 张，松开，等 0.6 秒，再跑 F2';
})()
```

### 5.3 读数 —— `F2`

```js
// F2: read the drag probe
(() => {
  const p = window.__probe;
  if (!p) return 'FAIL: probe not armed (先跑 F1)';
  p.stop = true;
  const moved = p.samples.filter(x => x.tf && (Math.abs(x.tf.tx) > 0.5 || Math.abs(x.tf.ty) > 0.5));
  const uniq = [...new Set(moved.map(x => x.tf.tx.toFixed(2) + ',' + x.tf.ty.toFixed(2)))];
  // 行距参照：相邻两张 pending 卡的顶边间距（= 卡片高 + section gap）
  const cards = [...document.querySelectorAll('.todo-section-pending .todo-card')];
  let rowPitch = 0;
  for (let i = 1; i < cards.length; i++) {
    const d = cards[i].getBoundingClientRect().top - cards[i-1].getBoundingClientRect().top;
    if (d > 0) { rowPitch = d; break; }
  }
  // 位移幅值 —— 只看"有几个不同的非零值"不够：FLIP 偏移被截断时照样有多个
  // 不同的非零值，只是幅值很小 → 会被误判成滑动。必须同时看幅值。
  const maxAbs = moved.length
    ? Math.max(...moved.map(x => Math.max(Math.abs(x.tf.tx), Math.abs(x.tf.ty))))
    : 0;
  const amplitudeOK = rowPitch > 0 ? maxAbs >= rowPitch * 0.7 : null;
  let verdict;
  if (!moved.length) {
    verdict = 'FAIL: SNAP — 瞬移，transform 被锁死或 FLIP 失效';
  } else if (uniq.length < 3) {
    verdict = 'WEAK: 只有 ' + uniq.length + ' 个不同位移值';
  } else if (amplitudeOK === false) {
    verdict = 'WEAK: 位移被截断 — maxAbs=' + maxAbs.toFixed(2) + 'px < 行距的 70%（'
             + (rowPitch * 0.7).toFixed(2) + 'px）';
  } else {
    verdict = 'PASS: SLIDING — 归位是滑动，且位移幅度完整';
  }
  return {
    cardId: p.id,
    framesSampled: p.samples.length,
    framesWithTranslation: moved.length,          // ← 断言：> 0
    distinctTranslations: uniq.length,            // ← 断言：>= 3
    maxAbsTranslation: Number(maxAbs.toFixed(2)), // ← 断言：≈ rowPitch
    rowPitch: Number(rowPitch.toFixed(2)),
    amplitudeOK,                                  // ← 断言：true
    sampleValues: uniq.slice(0, 12),
    firstMoved: moved.slice(0, 4).map(x => x.tf),
    lastMoved: moved.slice(-4).map(x => x.tf),
    verdict,
  };
})()
```

**预期输出（通过）**

```text
{
  cardId: 'a1b2c3…',
  framesSampled: 180,
  framesWithTranslation: 47,
  distinctTranslations: 9,
  maxAbsTranslation: 18.4,        // ≈ 一个行距
  rowPitch: 44.5,
  amplitudeOK: true,
  sampleValues: ['0.00,-18.40', '0.00,-12.27', '0.00,-6.13', '0.00,-0.00', '0.00,6.13', '0.00,12.27', '0.00,18.40', ...],
  firstMoved: [{tx: 0, ty: -18.4}, {tx: 0, ty: -12.27}, {tx: 0, ty: -6.13}, {tx: 0, ty: -0.01}],
  lastMoved:  [{tx: 0, ty: 6.13}, {tx: 0, ty: 12.27}, {tx: 0, ty: 18.4}, {tx: 0, ty: 18.4}],
  verdict: 'PASS: SLIDING — 归位是滑动，且位移幅度完整'
}
```

**失败签名 A（旧实现：心跳 animation 锁死 transform）**

```text
{
  framesWithTranslation: 0,          // ← 一次都没有
  distinctTranslations: 0,
  verdict: 'FAIL: SNAP — 瞬移，transform 被锁死或 FLIP 失效'
}
```

> 旧实现下 computed transform 会被心跳 keyframes 钉在 `matrix(1, 0, 0, 1, 0, 0)`
> （`rotate(0deg)` 不产生平移分量）→ `tx/ty` 恒为 0 → `framesWithTranslation` 恒为 0。判据非常干净。

**失败签名 B（样式表 transform 过渡吃掉 FLIP：v0.2.10 的中间状态）**

```text
{
  framesWithTranslation: 14,        // ← 有非零值！但不是 0
  distinctTranslations: 6,          // ← >= 3，看起来像"滑动"
  maxAbsTranslation: 1.7,          // ← 但幅值只有行距的 ~4%
  rowPitch: 44.5,
  amplitudeOK: false,              // ← 关键判据
  verdict: 'WEAK: 位移被截断 — maxAbs=1.70px < 行距的 70%（31.15px）'
}
```

> 这个签名**只看"有几个不同的非零位移值"会漏判**（6 ≥ 3、1.7px > 0.5px 阈值），
> 但用户看到的仍是瞬移。`maxAbsTranslation` / `amplitudeOK` 才是判据。
> 根因：`.todo-card` 的 transition 里留着 `transform` → SortableJS 第 1 步
> `css(target,'transition','')` 只清 inline，清不掉样式表的 → 第 3 步"瞬时摆回
> 旧视觉位置"变成一段被中途改向的过渡 → FLIP 偏移从未完整落地。
> **v0.2.10b 的修法**：把 `transform` 从 `.todo-card` 的 transition 列表里去掉
> （只剩 `opacity`），transform 过渡交回 SortableJS 的 inline transition 独占。

### 5.4 纯肉眼判据（不用 console 也能看）

拖起一张卡，让它**经过**其它卡上方，观察**其它卡**：

- ✅ **通过**：其它卡**连续滑动**让位（能看清中间过程，不是"啪"一下换位置）。
- ❌ **失败**：其它卡**直接消失再出现**（瞬移）。

配合 AGENTS.md 验收清单第 4 条（"松手后卡片是滑动归位，不是瞬移"）。

---

## 6. 判定项 5：逐卡玻璃与卡间缝隙

### `G`

```js
// G: per-tile glass + transparent seam between cards
(() => {
  const cards = [...document.querySelectorAll('.todo-section-pending .todo-card')];
  if (cards.length < 2) return 'NEED >= 2 pending cards';
  const bfOf = el => { if (!el) return '(missing)'; const cs = getComputedStyle(el); return cs.backdropFilter || cs.webkitBackdropFilter || 'none'; };
  const label = el => { const c = el.getAttribute && el.getAttribute('class'); return el.id ? '#' + el.id : (c ? '.' + c.trim().split(/\s+/).join('.') : el.tagName.toLowerCase()); };
  const a = cards[0], b = cards[1];
  const ra = a.getBoundingClientRect(), rb = b.getBoundingClientRect();
  const x = Math.round((ra.left + ra.right) / 2);
  const y = Math.round((ra.bottom + rb.top) / 2);          // 两卡之间的缝隙中线
  const stack = document.elementsFromPoint(x, y);
  const withBf = stack.filter(el => bfOf(el) !== 'none');
  return {
    cardBackdropFilters: cards.slice(0, 6).map(c => ({ id: c.dataset.todoId, bf: bfOf(c), willChange: getComputedStyle(c).willChange })),
    inputTileBf: bfOf(document.querySelector('.todo-input')),
    pasteBtnBf: bfOf(document.querySelector('.todo-paste-btn')),
    gap: { cardABottom: +ra.bottom.toFixed(1), cardBTop: +rb.top.toFixed(1), gapPx: +(rb.top - ra.bottom).toFixed(1) },
    probePoint: { x, y },
    stackAtGap: stack.map(label),
    elementsWithBackdropFilterAtGap: withBf.map(label),    // ← 断言：[]
    verdict: stack.length === 0
      ? 'INVALID: 探针点不在视口内（列表滚走了？）— 让这两张卡可见后重跑'
      : (withBf.length === 0
        ? 'PASS: 卡缝是真透明缝（缝上方没有任何 backdrop-filter 元素）'
        : 'FAIL: 玻璃跨过缝隙（' + withBf.map(label).join(', ') + '）'),
  };
})()
```

**预期输出**

```text
{
  cardBackdropFilters: [
    { id: 'a1b2…', bf: 'blur(10px) saturate(1.8)', willChange: 'backdrop-filter' },
    { id: 'c3d4…', bf: 'blur(10px) saturate(1.8)', willChange: 'backdrop-filter' },
    ...
  ],
  inputTileBf: 'blur(10px) saturate(1.8)',
  pasteBtnBf: 'blur(10px) saturate(1.8)',
  gap: { cardABottom: 63.5, cardBTop: 69.5, gapPx: 6 },     // ← 期望 6px（section 的 inline gap）
  probePoint: { x: 150, y: 66 },
  stackAtGap: ['.todo-section-pending', '.todo-list', '#app', 'body', 'html'],
  elementsWithBackdropFilterAtGap: [],                        // ← 断言
  verdict: 'PASS: 卡缝是真透明缝（缝上方没有任何 backdrop-filter 元素）'
}
```

**断言清单**
- 每张卡的 `bf` 都**非 `none`** → 玻璃是**逐卡**的（不是整窗一块）。
- `gapPx === 6` → 卡间确实有 6px 的物理间隙。
- `elementsWithBackdropFilterAtGap` 为 `[]` → **缝隙上方没有任何带 backdrop-filter 的元素**
  → 缝隙背后直接是桌面（这正是"圆角胶囊 + 透明间隙"观感的来源，也是"整窗单面板"方案会毁掉的东西）。
- `stackAtGap` 里**不出现** `.todo-card` → 探针点确实落在卡外。

> `elementsFromPoint` 跳过 `visibility:hidden` 和 `pointer-events:none` 的元素 —— 这正是本判据成立的基础，
> 也是 §7 判据 6 的基础。详见 §9 坑 7 / 坑 8。

---

## 7. 判定项 6：done 段保留区不铺玻璃

未 hover 时，done section 是 `opacity: 0` + `visibility: hidden` + `pointer-events: none`，
但**仍然占布局高度**。这一块区域**不应该有玻璃**（否则就是"底部一大片模糊留白"——
那正是被用户否决的 `perf/single-glass-panel` 方案的回归现象）。

### `H`

```js
// H: done reserved area must NOT be covered by any backdrop-filter element
(() => {
  const done = document.querySelector('.todo-section-done');
  const hdr = document.querySelector('.section-header-done');
  if (!done) return 'no .todo-section-done in DOM';
  const r = done.getBoundingClientRect();
  if (r.height < 1) return 'done section has zero height (没有已完成的 todo) — 无可测区域';
  const bfOf = el => { const cs = getComputedStyle(el); return cs.backdropFilter || cs.webkitBackdropFilter || 'none'; };
  const label = el => { const c = el.getAttribute && el.getAttribute('class'); return el.id ? '#' + el.id : (c ? '.' + c.trim().split(/\s+/).join('.') : el.tagName.toLowerCase()); };
  const probes = [];
  for (let i = 0; i <= 4; i++) {
    const y = Math.round(r.top + (r.height * i) / 4);
    const x = Math.round(r.left + r.width / 2);
    const stack = document.elementsFromPoint(x, y);
    probes.push({ y, stack: stack.map(label), withBackdropFilter: stack.filter(el => bfOf(el) !== 'none').map(label) });
  }
  const hr = hdr ? hdr.getBoundingClientRect() : null;
  const hdrStack = (hr && hr.height > 1)
    ? document.elementsFromPoint(Math.round(hr.left + hr.width / 2), Math.round(hr.top + hr.height / 2))
    : [];
  const hovered = !!document.body.dataset.hover;
  const clean = probes.every(p => p.withBackdropFilter.length === 0);
  const offScreen = probes.some(p => p.stack.length === 0);
  return {
    hoverActive: hovered,                       // ← 断言：false（鼠标必须在浮窗外）
    pinMode: document.body.dataset.pinMode ?? '(unset)',
    doneSection: {
      top: Math.round(r.top), height: Math.round(r.height), bottom: Math.round(r.bottom),
      opacity: getComputedStyle(done).opacity,          // '0'
      visibility: getComputedStyle(done).visibility,    // 'hidden'
      pointerEvents: getComputedStyle(done).pointerEvents,  // 'none'
    },
    doneHeader: hdr ? {
      opacity: getComputedStyle(hdr).opacity,
      color: getComputedStyle(hdr).color,                // 'rgba(0, 0, 0, 0)' —— idle 下 color:transparent，文字完全隐藏
      stackAtHeader: hdrStack.map(label),
    } : '(none)',
    probes,
    verdict: hovered
      ? 'INVALID: 鼠标在浮窗内（done 段是可见的）— 把鼠标移出浮窗、等 0.3s 让过渡走完，再重跑'
      : (offScreen
        ? 'INVALID: 有探针点落在视口外（done 段被滚出可视区？）— 滚动到能看到 done 段再重跑'
        : (clean ? 'PASS: done 保留区上方没有任何 backdrop-filter 元素' : 'FAIL: 有带 backdrop-filter 的东西盖在 done 保留区上')),
  };
})()
```

**预期输出（鼠标在浮窗外时）**

```text
{
  hoverActive: false,
  pinMode: 'pin_top',
  doneSection: { top: 118, height: 82, bottom: 200, opacity: '0', visibility: 'hidden', pointerEvents: 'none' },
  doneHeader: { opacity: '1', color: 'rgba(0, 0, 0, 0)', stackAtHeader: ['.section-header-done', '.todo-list', '#app', 'body', 'html'] },
  probes: [
    { y: 118, stack: ['.todo-list', '#app', 'body', 'html'], withBackdropFilter: [] },
    { y: 138, stack: ['.todo-list', '#app', 'body', 'html'], withBackdropFilter: [] },
    { y: 159, stack: ['.todo-list', '#app', 'body', 'html'], withBackdropFilter: [] },
    { y: 179, stack: ['.todo-list', '#app', 'body', 'html'], withBackdropFilter: [] },
    { y: 200, stack: ['.todo-list', '#app', 'body', 'html'], withBackdropFilter: [] }
  ],
  verdict: 'PASS: done 保留区上方没有任何 backdrop-filter 元素'
}
```

**断言清单**
- `hoverActive === false` → 必须先把鼠标移出浮窗（否则 done 段可见，探针会撞上真卡片 → 假 FAIL）。
- `doneSection.visibility === 'hidden'` + `opacity === '0'` + `pointerEvents === 'none'` → 保留区已退出绘制。
- 所有 `probes[].withBackdropFilter` 为 `[]` → 保留区**没有**玻璃。
- `probes[].stack` 里**不出现** `.todo-section-done` / `.todo-card` → WebKit 确实把隐藏子树踢出了命中测试
  （= 也不会合成它，`styles.css` v0.2.8 段的设计意图成立）。

**肉眼交叉验证**：未 hover 时，浮窗底部 done 保留区应该**直接透出桌面**（和卡间的透明缝一样干净），
**不应该**有一大块模糊的浅色区域。

---

## 8. 如果失败怎么办（备选方案，按代价排序）

> 以下三条**都需要改源文件**，本手册不代改。写清楚改法要点，供决策。

### 备选 ① —— 把哨兵从 1px 放大成**覆盖整个视口的隐形 backdrop-filter 面板**

**改法要点**（只改 `src/styles.css` 的 `.glass-keepalive` 一条规则）

```css
.glass-keepalive {
  position: fixed;
  inset: 0;                       /* ← 原来只有 top/left + width/height:1px */
  width: auto;
  height: auto;
  opacity: 0.001;
  pointer-events: none;
  z-index: -1;
  will-change: transform;
  backdrop-filter: blur(10px) saturate(180%);
  -webkit-backdrop-filter: blur(10px) saturate(180%);
  animation: usticky-backdrop-heartbeat 4s linear infinite;
}
```

**为什么它是"窗口级假设"的最强形式**：如果**单个覆盖全视口的 backdrop 层**都保不住采样，
那问题就不在"缺少活动层"这一层，①②③ 全都不对路 —— 该方向要重新想。
所以**先做 ① 验证假设，再考虑 ②③**。

**代价**
- 合成层数量仍是 **O(1)**（还是一个元素），但**每帧重采样的面积**从 1px² 变成整个视口
  → GPU 成本介于"逐卡心跳"和"整窗单面板"之间。这正是"整窗单面板"方案 GPU 成本偏高的一半原因。
- 这一层自带 `backdrop-filter`，虽然 `opacity: 0.001` 让它肉眼不可见，但它**真的会把整个窗口背景模糊一遍**
  再以 0.001 的 alpha 叠上去 → 卡缝的"完全透明"会被这层 0.001 的模糊**轻微**破坏（可忽略，但 §6 的 `G` 段
  判据可能出现 `elementsWithBackdropFilterAtGap` 非空 —— 因为哨兵 `pointer-events:none`，它**不会**出现在
  `elementsFromPoint` 栈里，所以判据仍然成立；但**视觉上**卡缝可能不再是 100% 透明）。

### 备选 ② —— 恢复逐卡心跳，但**只动画 `opacity`、不动画 `transform`**

**改法要点**

```css
/* 1) keyframes 去掉 transform，只留 opacity —— 这是本方案的全部意义 */
@keyframes usticky-backdrop-heartbeat {
  0%, 100% { opacity: 0.001; }
  50%      { opacity: 0.002; }
}

/* 2) 动画挂回瓦片 */
.todo-card,
.todo-input,
.todo-paste-btn {
  animation: usticky-backdrop-heartbeat 4s linear infinite;
}
```

**对 SortableJS 归位滑动的影响**
- ✅ **`transform` 不再被锁** → SortableJS 的 inline `transform` + 150ms 过渡**正常生效** → 归位滑动正常。
  （经验 ⑦ 的坑只针对"animation 声明了的那个属性"。）
- ⚠️ **`opacity` 被锁死** → 同一元素上一切依赖 opacity 的效果失效：
  - `.todo-card.dragging { opacity: 0.5 }` → 拖拽态的半透明**消失**（卡片看起来不淡）。
  - `.todo-card.vanishing` 的 opacity 淡出 → 只剩 `max-height` 塌缩，**没有淡出**。
  - `.todo-card::before` 的 `opacity` 过渡 → **不受影响**（伪元素自己没挂 animation）。
    ✅ 这是最关键的好消息：`styles.css` 早就把玻璃背景搬到了 `::before` 上
    （"伪元素不带 backdrop-filter / will-change / 心跳，background-color 过渡走 paint 通道"），
    所以**玻璃背景的 hover 渐变本来就是安全的**。
- **补救办法（推荐）**：把"卡片自身的淡入淡出"全部**搬到 `::before` 上**，因为 `::before` 永远不挂 animation：
  ```css
  /* 拖拽半透明：改 ::before 的背景不透明度，而不是卡片自身的 opacity */
  .todo-card.dragging::before { opacity: 0.15; }        /* 原来 .dragging { opacity: .5 } */
  /* 消失淡出：同理 */
  .todo-card.vanishing::before { opacity: 0; }
  .todo-card.dragging,
  .todo-card.sortable-chosen { transform: scale(0.98); }  /* transform 不被锁，安全 */
  ```
  注意 `.todo-card::before` 已有 `transition: opacity 0.22s …`，直接复用即可。
- **代价**：**又回到 O(N) 个动画层** → 每帧 N 次 backdrop 重采样 → **GPU 风暴回来了**
  （这正是 v0.2.10 要消灭的"卡顿感"的另一半）。所以 ② 是"退回旧架构但避开一个坑"，**不是免费的**。
  只有在 ① 也失败、且能接受 O(N) 成本时才考虑。

### 备选 ③ —— 回退到"整窗单玻璃面板"（分支 `perf/single-glass-panel`，commit `b334114`）

> ⚠️ **这个方案已经被用户明确否决，不要合。**

用户实测反馈（AGENTS.md v0.2.10 段原文）：

1. **未 hover 时底部一大片背景模糊留白** —— 单面板是整窗矩形，把 done 段的保留区也盖住了。
2. **hover 后玻璃覆盖不足** —— 底边约 95px 无玻璃，且固定面板**不跟随滚动内容**。
3. **卡间透明缝隙消失** —— 原来"圆角胶囊 + 透明间隙"的观感没了（单块连续玻璃在物理上切不出缝）。

**结论**：③ 是"已知会引入 3 个视觉回归"的回退，**不是安全选项**。
选 ③ 之前**必须先跟用户确认接受这 3 个回归**（而且 `b334114` 在另一个分支上，
v0.2.10 之后的改动要手工合并，不是简单 cherry-pick）。

### 决策树

```
A/B 实验结果？
├─ 结果 A（玻璃掉了，且 force-refresh 能救回来）
│   → 假设成立 → v0.2.10 方案成立 → 走用户视觉验收（AGENTS.md v0.2.10 验收清单 6 条）→ 合 main
│
├─ 结果 B（玻璃不掉）
│   → 先做 §4.5 的三条补充排查（B-1 心跳有无用 / L1 是否已足够 / 是不是被遮挡导致）
│     ├─ 确认"根本没有节流" → L2 心跳整层可删（甚至整个哨兵可删）→ 简化实现 + 零 GPU 成本 ★最优
│     ├─ 确认"只有 PinBottom + 被遮挡时才有问题" → 那是另一个 bug（合成层被丢弃），不是 backdrop 节流
│     └─ 仍不确定 → 试备选 ①（改 1 条 CSS 规则，代价可控）
│
├─ 结果 B'（玻璃掉了但 force-refresh 救不回来）
│   → 不是采样被丢弃 → 查 `D` 段的 tilesWithAnimation / `G` 段的 cardBackdropFilters
│     → 查 `body[data-low-power]`（会 blanket 关掉 backdrop-filter）
│
└─ 结果 C（恢复哨兵也回不来）
    → 采样丢失状态是"粘"的 → 备选 ① → 备选 ② → （需用户点头）备选 ③
```

---

## 9. WebKit 特有的坑（写片段时踩过的 / 会踩的）

| # | 坑 | 现象 | 规避 |
|---|---|---|---|
| 1 | **`document.getAnimations()` 的可用性** | Web Animations API 的 `Document.getAnimations()` 是 **Safari 13.1+（macOS 13.1）** 才有的。Tauri 2 只要求 macOS 10.15+，理论上 10.15.0/10.15.1 的 WKWebView **没有**这个 API → `typeof document.getAnimations === 'function'` 为 false，直接调会抛 `TypeError` | 一律用 `typeof document.getAnimations === 'function'` 守卫；提供 `getComputedStyle` fallback（§2.3）。本机 macOS 26 一定有，但别假设 |
| 2 | **`animationName` 不是所有 Animation 都有** | `document.getAnimations()` 会把**正在跑的 CSS transition** 也列进来。`CSSTransition` 对象没有 `animationName`（是 `undefined`），`CSSAnimation` 才有 | 过滤 `a => !!a.animationName`；断言用"**唯一的 CSS 动画名集合**"，别断言总数 |
| 3 | **`prefers-reduced-motion: reduce`** | macOS「减弱动态效果」打开时，`matchMedia('(prefers-reduced-motion: reduce)').matches` 为 true。WebKit 会对某些动画做抑制/降级 → `C` 段可能判成 `DEAD`，而实际只是被系统压制 | `A` 段已报这个值。若为 true，**先关掉系统的「减弱动态效果」**再重跑 |
| 4 | **`Animation.currentTime` 可能是 `CSSUnitValue`** | 启用 Typed OM 的 WebKit 里，`currentTime` 返回 `CSSUnitValue` 对象而不是 number → `t1 > t0` 比较对象会得到错误结果 | 归一：`const num = v => (v && typeof v === 'object' && 'value' in v) ? Number(v.value) : Number(v)`（`C` 段已内建） |
| 5 | **`saturate(180%)` 的 computed 值是 `saturate(1.8)`** | 写断言时别写死 `180%` | 断言用 `!== 'none'` 或 `/saturate\(/` |
| 6 | **`getComputedStyle(el).backdropFilter` 在老 WebKit 上可能是空串** | 未加 `-webkit-` 前缀属性读出来是 `''` 而不是 `'none'` → `!== 'none'` 判定会把"空串"当成"有滤镜" | 一律用 `cs.backdropFilter \|\| cs.webkitBackdropFilter \|\| 'none'` |
| 7 | **`elementsFromPoint` 跳过 `visibility:hidden` 和 `pointer-events:none` 的元素** | 期望出现在栈里的元素没出现 → 先查这两条属性，而不是查坐标 | §6 / §7 的判据**正是**靠这条成立的（隐藏的 done 段理应不在栈里） |
| 8 | **`elementsFromPoint` 只反映"命中测试"，不反映"绘制"** | 它**不能**直接证明"这块区域没有玻璃"，只能证明"最上层的可命中元素没有 backdrop-filter"。要证明"没绘制"，得配合 `visibility:hidden`（WebKit 会跳过绘制/合成/命中测试） | §7 的证据链是「`visibility:hidden` + 栈里无滤镜元素」两条合起来才完整 |
| 9 | **`::before` 伪元素不出现在 `elementsFromPoint`**（部分实现也不出现在 `getAnimations()`） | 计数断言要留余量 | 断言针对**唯一动画名集合**，不针对元素总数 |
| 10 | **DOM 变更本身会触发一次 invalidation** | 移除哨兵的**那一刻**玻璃一定会被重新采样一次 → 看起来"什么都没坏" | 计时起点定在"鼠标移回浮窗之后"，等 **≥6s**（2s 节流窗口 × 3） |
| 11 | **PinBottom 的 hover-raise 会触发 L3 重采样** | `platform/macos.rs` 的 hover emitter 在 PinBottom 下切 NSWindow level → emit `usticky://backdrop-refresh` → `main.ts` 给可见卡加 `.force-reflow` → **强制重采样玻璃** → 会把失败结果洗成假阴性 | 切到 **PinTop**（或 Normal）做实验 |
| 12 | **窗口被完全遮挡时 WKWebView 会暂停合成 / 节流 rAF** | PinBottom 的浮窗在其他 app 窗口**之下** → 被盖住时 rAF 停摆 → `F1` 的探针采不到样本，`C` 段可能判成 DEAD | 用 PinTop；确保浮窗可见、不被遮挡 |
| 13 | **Tauri devtools 默认 docked 在应用窗口里** | 鼠标一移到 console，浮窗就失焦 / 失 hover → `body[data-hover]` 被清 → 玻璃强弱随鼠标进出变化，实验读不出来 | DevTools `⋮` → Dock side → **Undock into separate window**，摆在浮窗旁边 |
| 14 | **`el.style.transform` ≠ `getComputedStyle(el).transform`** | inline 是**目标值**（瞬间跳），computed 才是**插值中的实际值**。"滑动 vs 瞬移"的差别**只在 computed 里看得见** | §5 全部读 computed |
| 15 | **2D / 3D 变换的 computed 序列化不同** | `translate3d(...)` 在 WebKit 里序列化成 `matrix3d(...)`（平移在 index **12 / 13**），2D 是 `matrix(...)`（平移在 index **4 / 5**） | 解析器两种都要支持（`F1` 段已处理） |
| 16 | **`opacity` 的 computed 值可能被截断** | `0.001` 与 `0.0011` 在某些精度下会被截成同一个字符串 | liveness 判定以 `transform` 为准，`opacity` 只是辅助（`C` 段的 `advanced` 或 `changed` 任一为真即可） |
| 17 | **`body[data-low-power] *` 是 blanket 规则** | `animation: none !important` + `backdrop-filter: none !important` → **哨兵的动画也被一起关掉** | 实验前确认 `lowPower: 'off'`（`A` 段） |

---

## 10. 结论记录表

跑完请把下表填好回传。

```
日期：____________   环境：macOS ______ / Safari ______   pin mode：________
devtools：docked / undocked          背景：有纹理 / 纯色

[ ] A  环境自检           pinMode=____  lowPower=____  hasGetAnimations=____  pending=____ done=____
[ ] B  哨兵静态快照       cssAnimationNames=____________________  heartbeatCount=____  computedBackdropFilter=____________
[ ] C  哨兵运行时活性     currentTimeAdvanced=____  computedChanged=____  verdict=____________________

[ ] D  卡片零 animation   tilesWithAnimation=____  allCssAnimationNames=____________________
                            sampleCardTransition=______________________________________________

[ ] E  核心 A/B
      E1 移除哨兵 → 鼠标移回 → 等 6s
          玻璃掉了？ 是 / 否        掉的时间点：____ 秒
      E3 阳性对照 force-reflow → 玻璃回来了？ 是 / 否
      E2/E2b 恢复哨兵 → 玻璃回来了？ 是 / 否        之后还会掉吗？ 是 / 否
      判定结果： 结果 A / 结果 B / 结果 B' / 结果 C

[ ] F  归位滑动           distinctTranslations=____  framesWithTranslation=____
                            verdict=______________________________________________

[ ] G  逐卡玻璃 + 卡缝    每卡 bf=____________  gapPx=____  elementsWithBackdropFilterAtGap=____
                            verdict=______________________________________________

[ ] H  done 保留区        hoverActive=____  doneSection.visibility=____
                            probes[].withBackdropFilter=____________________
                            verdict=______________________________________________

总体结论： 假设成立（可合 main） / 需要改代码（走 §8 决策树第 ____ 条）
遗留问题：____________________________________________________________
```

---

## 附：本文档涉及的所有命令速查

```js
// A  环境自检
// B  哨兵静态快照
// C  哨兵运行时活性（await，~0.6s）
// C-fallback  无 getAnimations 时的活性检查
// D  卡片零 animation
// E1 移除哨兵
// E2 恢复哨兵（不刷新）
// E2b location.reload()
// E3 阳性对照：force-reflow
// B-1 停掉心跳动画但保留元素 / 恢复
// F1 布防拖拽探针（先跑，再拖）
// F2 读拖拽探针
// G  逐卡玻璃 + 卡缝
// H  done 保留区不铺玻璃
```
