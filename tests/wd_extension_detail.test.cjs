// 运行：node --test tests/wd_extension_detail.test.cjs
// 详情栏里 WD 角色扩展给出的项（#550）：归属行和「也可能是」的悬停提示写「WD 扩展 0.93」，名字后面常显小字「扩展」
// （修饰类 .identity.ext、.alt.ext）；没有扩展证据时，详情栏的 HTML 和改动前一样。
const {test, before} = require('node:test');
const assert = require('node:assert/strict');
const web = require('./web.cjs');
let page, detail, state;
before(async () => {
  await web.ready();
  page = web.fakePage();
  page.document.addEventListener = () => {};
  globalThis.window = {addEventListener() {}};
  globalThis.ResizeObserver = class { observe() {} };
  globalThis.requestAnimationFrame = () => 0;
  globalThis.innerWidth = 1000; globalThis.innerHeight = 800;
  const store = new Map();
  globalThis.localStorage = {getItem: k => store.has(k) ? store.get(k) : null, setItem: (k, v) => store.set(k, v), removeItem: k => store.delete(k)};
  (await web.load('ui/grid.js')).wireGrid();
  detail = await web.load('ui/detail.js');
  state = web.core().state;
});
// 只回 /api/detail；其余读取（画师、统计）一律 404，详情栏照样画出角色那一节。
async function render(d) {
  globalThis.fetch = async path => path.startsWith('/api/detail')
    ? {status: 200, ok: true, headers: {get: () => null}, json: async () => ({uuid: 'a', memberships: [], alternatives: [], tags: [], user_tags: [], ...d})}
    : {status: 404, ok: false, headers: {get: () => null}, json: async () => ({error: 'stop'})};
  state.items = [{uuid: 'a'}]; state.total = 1;
  await detail.inspect('a');
  return page.el('inspector').innerHTML;
}
const machine = (id, name, evidence) => ({id, name, status: 'machine', origin: 'machine', variants: [], evidence});

test('扩展补上的机器归属：提示写分数，名字后面有「扩展」', async () => {
  const html = await render({memberships: [machine('c1', 'Newer', {sources: ['WD_EXT'], max_score: 0.9312})]});
  assert.match(html, /<div class="identity ext"><span class="dot machine" title="机器识别 · WD 扩展 0.93"><\/span><a class="identity-name"[^>]*>Newer<\/a><small>扩展<\/small>/);
});

test('人工确认过的不再标「扩展」', async () => {
  const html = await render({memberships: [{...machine('c1', 'Newer', {sources: ['WD_EXT'], max_score: 0.93}), status: 'confirmed', origin: 'human'}]});
  assert.match(html, /<div class="identity"><span class="dot confirmed"/);
  assert.doesNotMatch(html, /<small>扩展<\/small>/);
});

test('「也可能是」里的扩展项：和 WD、外部索引并列，原有各项不变', async () => {
  const html = await render({memberships: [machine('c0', 'Old', {sources: ['WD'], max_score: 0.95})], alternatives: [
    {id: 'c2', name: 'Strong', weak: false, evidence: {sources: ['CCIP', 'WD'], max_score: 0.9, wd: 0.91, ccip: 0.123}},
    {id: 'c3', name: 'Ext', weak: false, evidence: {sources: ['WD_EXT'], max_score: 0.876, wd: null, ccip: null}},
    {id: 'c4', name: 'Weak', weak: true, evidence: {sources: ['CCIP'], max_score: 0.1, wd: null, ccip: 0.1}}]});
  assert.match(html, /<button class="alt" data-alt="0" title="Strong · WD 0.91 · 外部索引 0.123。点击：换成这个；⌥ 点击：也是"><kbd>1<\/kbd>Strong<\/button>/);
  assert.match(html, /<button class="alt ext" data-alt="1" title="Ext · WD 扩展 0.88。点击：换成这个；⌥ 点击：也是"><kbd>2<\/kbd>Ext<small>扩展<\/small><\/button>/);
  assert.match(html, /<button class="alt weak" data-alt="2" title="Weak · 弱线索（中档相似，不算归属） · 外部索引 0.100。点击：换成这个；⌥ 点击：也是"><kbd>3<\/kbd>Weak<\/button>/);
  // WD 主归属不标扩展。
  assert.match(html, /<div class="identity"><span class="dot machine" title="机器识别 · WD"><\/span>/);
});

test('没有扩展证据时不出现扩展的类名和小字', async () => {
  const html = await render({memberships: [machine('c0', 'Old', {sources: ['CCIP', 'PROTO'], max_score: 0.5})],
    alternatives: [{id: 'c2', name: 'Strong', weak: false, evidence: {sources: ['WD'], max_score: 0.9, wd: 0.91, ccip: null}}]});
  assert.doesNotMatch(html, / ext"|<small>扩展|WD 扩展/);
  assert.match(html, /title="机器识别 · 外部索引 \+ 你的标注"/);
  assert.equal(detail.fromExt({sources: ['WD_EXT', 'CCIP']}), false, '和别的来源一起给出的不算只由扩展给出');
});
