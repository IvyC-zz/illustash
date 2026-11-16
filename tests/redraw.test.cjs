// 运行：node --test tests/redraw.test.cjs
// 「变了才重画」和按钮忙碌状态（#429）：ui/dom.js 的 redraw、setDrawn、busy。
const {test, before} = require('node:test');
const assert = require('node:assert/strict');
const web = require('./web.cjs');
let dom;
before(async () => { await web.ready(); dom = await web.load('ui/dom.js'); });
const card = () => { const el = {writes: 0, contains: x => !!x}; let html = ''; Object.defineProperty(el, 'innerHTML', {get: () => html, set: v => { html = v; el.writes++; }}); return el; };

test('unchanged markup is not written again; a forgotten element always is', () => {
  globalThis.document = {activeElement: null};
  const el = card();
  assert.equal(dom.redraw(el, 'one'), true);
  assert.equal(dom.redraw(el, 'one'), false);
  assert.equal(el.writes, 1);
  assert.equal(dom.drawnHTML(el), 'one');
  dom.setDrawn(el);
  assert.equal(dom.redraw(el, 'one'), true);
  assert.equal(el.writes, 2);
  // 别处整个改写过：记下现在的内容，相同就不重画。
  dom.setDrawn(el, 'two');
  assert.equal(dom.redraw(el, 'two'), false);
  assert.equal(el.writes, 2);
});

test('an input being edited is not wiped, unless the caller does not care', () => {
  globalThis.document = {activeElement: {matches: sel => sel.includes('input')}};
  const el = card();
  assert.equal(dom.redraw(el, 'one', {keepInput: true}), false);
  assert.equal(el.writes, 0);
  assert.equal(dom.redraw(el, 'one'), true);
  globalThis.document = {activeElement: {matches: () => false}};
  assert.equal(dom.redraw(el, 'two', {keepInput: true}), true);
});

test('a busy button comes back after success, cancel or failure (#429)', async () => {
  const a = {disabled: false}, b = {disabled: false};
  let seen;
  assert.equal(await dom.busy([a, b], async () => { seen = [a.disabled, b.disabled]; return 'done'; }), 'done');
  assert.deepEqual(seen, [true, true]);
  assert.deepEqual([a.disabled, b.disabled], [false, false]);
  // 取消（关掉选择框或确认框）：什么也没变，不会重画，按钮照样恢复。
  assert.equal(await dom.busy(a, async () => false), false);
  assert.equal(a.disabled, false);
  await assert.rejects(dom.busy(a, async () => { throw new Error('offline'); }), /offline/);
  assert.equal(a.disabled, false);
});
