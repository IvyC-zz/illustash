// 启动画面的时钟（web/static/splash.js，#480）：动画按逻辑时间播放，每帧最多前进 50 毫秒，加载中卡一下不会跳帧；
// 但 App 准备好（ready()）之后的退出、桌面外壳等待的 settled() 要按实际时间收尾，帧率低、页面隐藏（没有帧）、
// 减少动态效果时都不能无限拖延。没准备好时启动画面一直留着。
// splash.js 是经典脚本，在独立的 vm 上下文里运行：帧回调、计时器和 performance.now() 都由测试推进。DOM 是什么都接受的
// 普通对象，只记下启动画面有没有被移除。
const {test} = require('node:test');
const assert = require('node:assert/strict');
const path = require('node:path');
const vm = require('node:vm');
const {readFileSync} = require('node:fs');
const SOURCE = readFileSync(path.join(__dirname, '../web/static/splash.js'), 'utf8');

function element(text = '') {
  const el = {
    style: {}, children: [], textContent: text, innerHTML: '', offsetHeight: 128, removed: false, listeners: {},
    setAttribute() {}, getAttribute: () => '', hasAttribute: () => false, appendChild: c => c, insertBefore: c => c,
    addEventListener(type, fn) { (el.listeners[type] ||= []).push(fn); },
    querySelector: () => element(), remove() { el.removed = true; },
    getTotalLength: () => 100, getPointAtLength: d => ({x: 400 + 300 * Math.cos(d / 16), y: 600 + 300 * Math.sin(d / 16)}),
    getBoundingClientRect: () => ({left: 0, top: 0, width: 32, height: 32}),
  };
  el.parentElement = {offsetHeight: 400};
  return el;
}

// 运行一份 splash.js。`fps` 是帧率（0：没有帧，如页面隐藏），`reduce` 是减少动态效果，`launched` 是外壳打开的图库页。
// 返回 {root, splash, advance(ms), now(), fps(n)}：advance 按时间顺序依次执行帧和计时器。
function run({fps = 60, reduce = false, launched = false, shell = false} = {}) {
  let now = 1000, frame = null, nextFrame = 0, rate = fps;
  const timers = [];
  const root = element(); root.hasAttribute = k => shell && k === 'data-shell';
  const name = element('Illustash');
  root.querySelector = sel => sel === '.sp-name' ? name : element();
  const context = {
    document: {getElementById: id => id === 'splash' ? root : null, querySelector: () => null, createElementNS: () => element()},
    location: {search: launched ? '?launched' : ''}, URLSearchParams, Math, Number, Array, Object, Promise,
    matchMedia: () => ({matches: reduce}), performance: {now: () => now},
    requestAnimationFrame: fn => { frame = fn; if (!nextFrame && rate) nextFrame = now + 1000 / rate; return 1; },
    setTimeout: (fn, ms = 0) => { timers.push({at: now + ms, fn}); return timers.length; }, clearTimeout: id => { if (timers[id - 1]) timers[id - 1].fn = null; },
    t: s => s,
  };
  context.window = context;
  vm.runInNewContext(SOURCE, context);
  const advance = ms => {
    const end = now + ms;
    for (;;) {
      const timer = timers.filter(x => x.fn).sort((a, b) => a.at - b.at)[0];
      const at = Math.min(rate && frame && nextFrame ? nextFrame : Infinity, timer ? timer.at : Infinity);
      if (at > end) break;
      now = at;
      if (timer && timer.at === at) { const fn = timer.fn; timer.fn = null; fn(); continue; }
      const fn = frame; frame = null; nextFrame = 0; fn(now);
      if (frame && rate) nextFrame = now + 1000 / rate;
    }
    now = end;
  };
  return {root, splash: context.splash, advance, now: () => now,
    fps(n) { rate = n; nextFrame = n && frame ? now + 1000 / n : 0; }};
}
// 从现在起到启动画面被移除经过的毫秒数，最多等 limit。
function exitAfter(s, limit = 30000) {
  const start = s.now();
  for (let waited = 0; waited < limit && !s.root.removed; waited += 50) s.advance(50);
  return s.root.removed ? s.now() - start : Infinity;
}
// 动画本身的退出最长约 2.5 秒（最后一次翻卡 + 飞向侧栏），再加一帧和余量。
const EXIT = 3000;

test('60 帧：准备好后正常播完退出动画再移除', () => {
  const s = run(); s.advance(5000); s.splash.ready();
  const ms = exitAfter(s);
  assert.ok(ms > 500 && ms <= EXIT, `退出用了 ${ms} 毫秒`);
});
test('没准备好时启动画面一直留着，帧和计时器都照常走', () => {
  for (const fps of [60, 5, 0]) {
    const s = run({fps}); s.advance(60000);
    assert.equal(s.root.removed, false, `${fps} 帧`);
  }
});
for (const fps of [10, 5, 2]) {
  test(`${fps} 帧：准备好后按实际时间退出，不因每帧 50 毫秒的上限拖长`, () => {
    const s = run(); s.advance(5000); s.fps(fps); s.splash.ready();
    const ms = exitAfter(s);
    assert.ok(ms <= EXIT + 1000 / fps, `退出用了 ${ms} 毫秒`);
  });
}
test('准备好时帧率已经很低（加载期间逻辑时间落后）：退出仍按实际时间', () => {
  const s = run({fps: 4}); s.advance(20000); s.splash.ready();
  const ms = exitAfter(s);
  assert.ok(ms <= EXIT + 250, `退出用了 ${ms} 毫秒`);
});
test('准备好后页面隐藏（没有帧）：到时间也移除启动画面', () => {
  const s = run(); s.advance(5000); s.splash.ready(); s.fps(0);
  const ms = exitAfter(s);
  assert.ok(ms <= EXIT + 1000, `退出用了 ${ms} 毫秒`);
});
test('隐藏期间准备好、恢复后：只播退出动画剩下的部分，不从头播', () => {
  const s = run(); s.advance(4000); s.fps(0); s.splash.ready(); s.advance(1500);
  assert.equal(s.root.removed, false);   // 隐藏 1.5 秒：还在退出动画的时间里
  s.fps(60);
  const ms = exitAfter(s);
  assert.ok(ms <= EXIT - 1500 + 100, `恢复后又用了 ${ms} 毫秒`);
});
test('减少动态效果：准备好后很快移除', () => {
  for (const fps of [60, 5]) {
    const s = run({reduce: true, fps}); s.advance(500); s.splash.ready();
    const ms = exitAfter(s);
    assert.ok(ms <= 1000 + 1000 / fps, `${fps} 帧，退出用了 ${ms} 毫秒`);
  }
});
test('一出现就准备好（图标还没成形）：照样播到图标成形再退出，按实际时间', () => {
  for (const fps of [60, 5]) {
    const s = run({fps}); s.advance(300); s.splash.ready();
    const ms = exitAfter(s);
    assert.ok(ms >= 3000 && ms <= 4200 + 1000 / fps, `${fps} 帧，退出用了 ${ms} 毫秒`);
  }
});

// 桌面外壳的启动页（desktop/dist/index.html 的 launch）等 settled() 才打开图库页。
async function settledAfter(s, limit = 30000) {
  let done = false; s.splash.settled().then(() => { done = true; });
  const start = s.now();
  for (let waited = 0; waited < limit && !done; waited += 50) { s.advance(50); await null; }
  return done ? s.now() - start : Infinity;
}
test('settled()：60 帧时等图标成形', async () => {
  const s = run({shell: true});
  const ms = await settledAfter(s);
  assert.ok(ms >= 3150 && ms <= 3300, `用了 ${ms} 毫秒`);
});
test('settled()：加载期间偶尔卡一下（0.4 秒）不跳帧，只是相应晚一点', async () => {
  const s = run({shell: true}); s.advance(1000); s.fps(0); s.advance(400); s.fps(60);
  const ms = await settledAfter(s) + 1400;
  assert.ok(ms >= 3400 && ms <= 3550, `用了 ${ms} 毫秒`);
});
test('settled()：图标已成形时立即解析', async () => {
  const s = run({shell: true}); s.advance(4000);
  assert.ok(await settledAfter(s) <= 50);
});
for (const fps of [5, 0]) {
  test(`settled()：${fps} 帧时也按实际时间解析`, async () => {
    const s = run({shell: true, fps});
    const ms = await settledAfter(s);
    assert.ok(ms <= 3200 + 1250, `用了 ${ms} 毫秒`);   // 图标成形 + 最多落后 0.5 秒 + 没有帧时每 0.25 秒走一次
  });
}
test('settled()：减少动态效果时立即解析', async () => {
  assert.ok(await settledAfter(run({shell: true, reduce: true})) <= 50);
});
