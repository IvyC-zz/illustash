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
