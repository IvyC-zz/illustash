// 运行：node --test tests/index_coverage.test.cjs
// 检查新角色按作品显示覆盖情况（#561）：「设置 › 角色资料」的「检查新角色」卡片里，上次检查的每个关注作品有几个已由
// 数据包覆盖、几个自己补过、还缺几个（GET /api/index/coverage，/api/index 的回复冻结所以另开路由）。
const {test, before} = require('node:test');
const assert = require('node:assert/strict');
const web = require('./web.cjs');
let ui;
before(async () => {
  await web.ready();
  web.fakePage();
  const store = new Map();
  globalThis.localStorage = {getItem: k => store.has(k) ? store.get(k) : null, setItem: (k, v) => store.set(k, v), removeItem: k => store.delete(k)};
  ui = await web.load('ui/settings.js');
});

test('合计写在折叠区标题，每个作品一行；没有回应的作品不列', () => {
  const html = ui.coverageFold({works: [{tag: 'work_a', name: '作品甲'}]}, {checked_at: 't', coverage: {
    work_a: {packs: 12, own: 1, missing: 2}, work_b: {packs: 0, own: 0, missing: 3}, work_c: null}});
  assert.match(html, /data-fold-key="indexCoverage"><summary>数据包覆盖 12 个 · 自己补过 1 个 · 还缺 5 个<\/summary>/);
  assert.match(html, /<tr><th>作品<\/th><th>数据包覆盖<\/th><th>自己补过<\/th><th>还缺<\/th><\/tr>/);
  assert.match(html, /<tr><td>作品甲<\/td><td>12<\/td><td>1<\/td><td>2<\/td><\/tr>/);
  assert.match(html, /<tr><td>work b<\/td><td>0<\/td><td>0<\/td><td>3<\/td><\/tr>/, '不在作品列表里的用 tag 推出名字');
  assert.doesNotMatch(html, /work_c|work c/);
});

test('还没检查过（路由回 null）或作品都没有回应：不显示', () => {
  assert.equal(ui.coverageFold({works: []}, null), '');
  assert.equal(ui.coverageFold({works: []}, {checked_at: 't', coverage: {work_a: null}}), '');
});

// #606：补角色以角色资料包为主。上次检查都不缺时检查新角色收进折叠区；还缺时资料包卡片底部写明还缺几个、给出入口；
// 不知道缺不缺时照常展开，资料包卡片上不写。
const idle = (extra = {}) => ({state: {status: 'idle'}, watch: {works: ['w1'], min_posts: 100}, check: {checked_at: 't', missing: []}, ...extra});
const none = {checked_at: '2026-11-18 10:00:00', coverage: {work_a: {packs: 12, own: 1, missing: 0}, work_c: null}};
const some = {checked_at: 't', coverage: {work_a: {packs: 12, own: 1, missing: 2}, work_b: {packs: 0, own: 0, missing: 3}}};

test('还缺几个：合计有回应的作品；不知道时为 null', () => {
  assert.equal(ui.coverageMissing(null), null);
  assert.equal(ui.coverageMissing({coverage: {work_a: null}}), null);
  assert.equal(ui.coverageMissing(none), 0);
  assert.equal(ui.coverageMissing(some), 5);
});

test('都不缺、有关注作品、没在跑或要处理时才折叠', () => {
  assert.equal(ui.indexFolded(idle(), none), true);
  assert.equal(ui.indexFolded(idle(), some), false);
  assert.equal(ui.indexFolded(idle(), null), false, '还没检查过');
  assert.equal(ui.indexFolded(idle({watch: {works: [], min_posts: 100}}), none), false, '没有关注作品');
  for (const status of ['checking', 'fetching', 'error', 'paused']) assert.equal(ui.indexFolded(idle({state: {status}}), none), false, status);
});

test('资料包卡片底部：还缺时写数字，有待添加的链到「作品」，否则给检查按钮', () => {
  assert.equal(ui.packCoverageLine(idle(), none), '');
  assert.equal(ui.packCoverageLine(idle(), null), '');
  const check = ui.packCoverageLine(idle(), some);
  assert.match(check, /关注的作品还缺 5 个角色<small class="line-hint">上次检查新角色时，角色资料包和本地都还没有<\/small>/);
  assert.match(check, /<button class="quiet small" data-index-go>检查新角色…<\/button>/);
  assert.match(ui.packCoverageLine(idle({state: {status: 'checking'}}), some), /data-index-go disabled/);
  const add = ui.packCoverageLine(idle({check: {checked_at: 't', missing: [{short_tag: 'x'}]}}), some);
  assert.match(add, /<a class="setting-line set-link" href="#\/characters\/works">.*查看并添加 ›<\/span><\/a>/);
});
