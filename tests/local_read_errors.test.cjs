const {test, before} = require('node:test');
const web = require('./web.cjs');   // readError() is core/session.js, readFailure() ui/dom.js (#199)
let readError, readFailure;
before(async () => { await web.ready(); ({readError} = web.core()); ({readFailure} = await web.load('ui/dom.js')); });
const assert = require('node:assert/strict');
const vm = require('node:vm');
const zh = require('./i18n_zh.cjs');   // t() of a Chinese page (#173)
const source = require('node:fs').readFileSync(require('node:path').join(__dirname, '../web/static/app.js'), 'utf8');
const twoStage = source.slice(source.indexOf('function twoStage('), source.indexOf('async function editProfile('));
const tick = () => new Promise(r => setImmediate(r));
test('a local search failure reports its cause and never queries remote or offers creation', async () => {
  let remoteCalls = 0, extraCalls = 0; const frames = [];
  const c = {readError}; vm.createContext(Object.assign(c, zh.page)); vm.runInContext(twoStage, c);
  c.twoStage(async () => { throw new Error('database locked'); }, async () => { remoteCalls++; }, () => { extraCalls++; return [{items: [{create: 'new'}]}]; })('name', f => frames.push(f));
  await tick(); assert.equal(remoteCalls, 0); assert.equal(extraCalls, 0);
  assert.equal(frames[0][0].note, '读取失败：database locked'); assert.equal(frames[0][0].items.length, 0);
});
test('inline failures show the escaped reason instead of an empty state', () => {
  const node = {hidden: true}; readFailure(node, new Error('<unreadable>'));
  assert.equal(node.hidden, false); assert.match(node.innerHTML, /role="alert"/); assert.match(node.innerHTML, /读取失败：&lt;unreadable&gt;/);
});
const shelfRead = source.slice(source.indexOf('async function renderShelf('), source.indexOf('// A watch toggle'));
const searchRead = source.slice(source.indexOf('async function searchIndex('), source.indexOf('// Replace only the sections'));
for (const [name, code, method] of [['works', shelfRead, 'renderShelf'], ['characters', searchRead, 'searchIndex']]) {
  test(`a late ${name} failure cannot expose its panel after navigation`, async () => {
    let reject; const nodes = {hint: {}, charSearch: {value: 'Alice'}, charIndex: {hidden: true}};
    const c = {state: {route: {page: 'characters', section: 'works'}}, session: {token: 'a'}, readError, $: id => nodes[id], drawShelf() {}, drawCast() {}, cast: {}, api: () => new Promise((_, r) => { reject = r; }), esc: s => s};
    vm.createContext(Object.assign(c, zh.page)); vm.runInContext(code, c);
    const pending = c[method](); c.state.route = {page: 'settings'}; reject(new Error('late error')); await pending;
    assert.equal(nodes.charIndex.hidden, true); assert.equal(nodes.charIndex.innerHTML, undefined);
  });
}
test('a late failed watch query cannot overwrite newer local results', async () => {
  const code = source.slice(source.indexOf('function wireWatchAdd('), source.indexOf('async function pollIndex('));
  const pending = []; const input = {value: 'A', setAttribute() {}}, menu = {};
  const c = {URLSearchParams, state: {}, session: {token: 'a'}, readError, $: id => id === 'watchAdd' ? input : menu, api: () => new Promise((resolve, reject) => pending.push({resolve, reject})), esc: s => s, workName: s => s, number: String, setTimeout: () => 0, clearTimeout() {}, document: {activeElement: null}, watchDraft: ''};
  vm.createContext(Object.assign(c, zh.page)); vm.runInContext(code, c); c.wireWatchAdd({watch: {works: []}}, () => {});
  input.onfocus(); input.value = 'B'; input.onfocus();
  pending[1].resolve([{id: 'b', name: 'B', tag: 'b', watchable: true}]); await tick();
  const current = menu.innerHTML; assert.match(current, />B</);
  pending[0].reject(new Error('old failure')); await tick(); assert.equal(menu.innerHTML, current);
});
