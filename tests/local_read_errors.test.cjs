const {test, before} = require('node:test');
const web = require('./web.cjs');   // readError() is core/session.js, readFailure() ui/dom.js (#199)
let readError, readFailure;
before(async () => { await web.ready(); ({readError} = web.core()); ({readFailure} = await web.load('ui/dom.js')); });
const assert = require('node:assert/strict');
const tick = () => new Promise(r => setImmediate(r));
test('a local search failure reports its cause and never queries remote or offers creation', async () => {
  // twoStage() is ui/palette.js: the local search first, then Danbooru.
  const {twoStage} = await web.load('ui/palette.js');
  let remoteCalls = 0, extraCalls = 0; const frames = [];
  twoStage(async () => { throw new Error('database locked'); }, async () => { remoteCalls++; }, () => { extraCalls++; return [{items: [{create: 'new'}]}]; })('name', f => frames.push(f));
  await tick(); assert.equal(remoteCalls, 0); assert.equal(extraCalls, 0);
  assert.equal(frames[0][0].note, '读取失败：database locked'); assert.equal(frames[0][0].items.length, 0);
});
test('inline failures show the escaped reason instead of an empty state', () => {
  const node = {hidden: true}; readFailure(node, new Error('<unreadable>'));
  assert.equal(node.hidden, false); assert.match(node.innerHTML, /role="alert"/); assert.match(node.innerHTML, /读取失败：&lt;unreadable&gt;/);
});
// The 资料库 is ui/catalog.js (#199); its reads go through the page's fetch.
for (const [name, method] of [['works', 'renderShelf'], ['characters', 'searchIndex']]) {
  test(`a late ${name} failure cannot expose its panel after navigation`, async () => {
    let reject; const nodes = {hint: {}, charSearch: {value: 'Alice'}, charIndex: {hidden: true}};
    const catalog = await web.load('ui/catalog.js'), {state, session} = web.core();
    Object.assign(state, {route: {page: 'characters', section: 'works'}, index: undefined}); session.token = 'a';
    globalThis.document = {getElementById: id => nodes[id]};
    globalThis.fetch = () => new Promise((_, r) => { reject = r; });
    const pending = catalog[method](); const drawn = nodes.charIndex.innerHTML;
    state.route = {page: 'settings'}; reject(new Error('late error')); await pending;
    assert.equal(nodes.charIndex.hidden, true); assert.equal(nodes.charIndex.innerHTML, drawn);
  });
}
test('a late failed watch query cannot overwrite newer local results', async () => {
  const {wireWatchAdd} = await web.load('ui/catalog.js'), {session} = web.core();
  const pending = []; const input = {value: 'A', setAttribute() {}}, menu = {};
  session.token = 'a';
  globalThis.document = {getElementById: id => id === 'watchAdd' ? input : menu, activeElement: null};
  globalThis.fetch = () => new Promise((resolve, reject) => pending.push({resolve: body => resolve({status: 200, ok: true, json: async () => body}), reject}));
  const setTimeout = globalThis.setTimeout; globalThis.setTimeout = () => 0;
  try {
    wireWatchAdd({watch: {works: []}}, () => {});
    input.onfocus(); input.value = 'B'; input.onfocus();
    pending[1].resolve([{id: 'b', name: 'B', tag: 'b', watchable: true}]); await tick(); await tick();
    const current = menu.innerHTML; assert.match(current, />B</);
    pending[0].reject(new Error('old failure')); await tick(); await tick(); assert.equal(menu.innerHTML, current);
  } finally { globalThis.setTimeout = setTimeout; }
});
