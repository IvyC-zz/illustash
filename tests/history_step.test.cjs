// #402：桌面 App 菜单的「后退／前进」只在这个页面自己的地址之间走（core/route.js 的 noteHistory、stepHistory）。
// 单独一个文件：序号从这个页面第一次记地址算起，同一文件里先跑别的换页测试会把它记过去。
const {test, before} = require('node:test');
const assert = require('node:assert/strict');
const web = require('./web.cjs');
before(web.ready);

test('后退只在本页的地址之间走：退到这次打开的第一条就停，不退回启动页', () => {
  const {noteHistory, stepHistory} = web.core();
  // 假的会话历史：第 0 条是启动页（另一个文档），之后是图库页的几条地址
  const entries = [{url: 'tauri://localhost/index.html', state: null}], saved = [globalThis.history, globalThis.location];
  let at = 0;
  const open = url => { entries.splice(at + 1); entries.push({url, state: null}); at = entries.length - 1; location.hash = url; noteHistory(); };
  globalThis.location = {hash: ''};
  globalThis.history = {
    get state() { return entries[at].state; },
    replaceState(state, _, url) { entries[at] = {url: url || entries[at].url, state}; },
    go(d) { const to = at + d; if (to < 0 || to >= entries.length) return; at = to; location.hash = entries[at].url; noteHistory(); },
  };
  try {
    open('#/home'); open('#/library'); open('#/settings');
    assert.deepEqual(entries.map(e => e.state?.illustash ?? null), [null, 0, 1, 2]);
    // 标题旁的「‹」按 canGoBack 显示；后退到的那条，noteHistory 说它是已有的记录（#526：按序号恢复滚动位置）
    const {canGoBack, historyEntry} = web.core();
    assert.deepEqual([canGoBack(), historyEntry()], [true, 2]);
    let traversed = null; const note = history.go;
    history.go = d => { at += d; location.hash = entries[at].url; traversed = noteHistory(); };
    stepHistory(-1); history.go = note;
    assert.deepEqual([traversed, historyEntry(), location.hash], [true, 1, '#/library']);
    stepHistory(1);
    stepHistory(-1); assert.equal(location.hash, '#/library');
    stepHistory(-1); assert.equal(location.hash, '#/home');
    stepHistory(-1); assert.equal(at, 1, '不退出这个页面');
    stepHistory(1); assert.equal(location.hash, '#/library');
    // 从中间打开新地址：序号接着当前这条往下记
    open('#/unmatched'); assert.equal(entries.at(-1).state.illustash, 2);
    stepHistory(1); assert.equal(location.hash, '#/unmatched');
    // 连按：第二下在第一下的地址变化到达之前，也不会越过第一条
    const go = history.go, queued = [];
    history.go = d => queued.push(d);
    stepHistory(-1); stepHistory(-1); stepHistory(-1); stepHistory(-1);
    history.go = go;
    assert.deepEqual(queued, [-1, -1], '从序号 2 最多退两步');
    for (const d of queued) history.go(d);
    assert.equal(at, 1);
  } finally { [globalThis.history, globalThis.location] = saved; }
});
