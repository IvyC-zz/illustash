const test = require('node:test');
const web = require('./web.cjs');   // core/ modules (#199)
test.before(web.ready);
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const zh = require('./i18n_zh.cjs');   // t() of a Chinese page (#173)
const source = fs.readFileSync('web/static/app.js', 'utf8');
const tick = () => new Promise(resolve => setImmediate(resolve));
const section = (a, b) => source.slice(source.indexOf(a), source.indexOf(b, source.indexOf(a)));
// The metadata read is core/meta.js; api answers the page's requests.
function metaHarness(api) {
  const {session, state, refreshMeta} = web.core();
  Object.assign(state, {nsfw: '', chars: [], dupInfo: {}, exclInfo: {}, route: {page: 'library'}, char: null, readEpoch: 0, countDirty: undefined});
  globalThis.fetch = path => Promise.resolve(api(path)).then(body => ({status: 200, ok: true, json: async () => body}));
  return {state, session, refreshMeta};
}
function value(path, name = 'current') {
  if (path.startsWith('/api/bootstrap')) return {token: name, undo: null, total: 1};
  if (path.startsWith('/api/identity/count')) return {total: 0};
  if (path.startsWith('/api/characters')) return [{id: name}];
  return [];
}
test('same-turn metadata requests coalesce, initial bootstrap is reused', async () => {
  const calls = [];
  const c = metaHarness(async path => {calls.push(path); return value(path);});
  const first = c.refreshMeta({boot: {token: 'seed', total: 1}});
  assert.equal(first, c.refreshMeta()); assert.equal(first, c.refreshMeta());
  await first;
  assert.equal(calls.length, 4);
  assert.ok(calls.every(p => !p.startsWith('/api/bootstrap')));
  assert.equal(c.session.token, 'seed');
});
test('in-flight changes trigger one trailing refresh and stale responses never apply', async () => {
  const pending = [];
  const c = metaHarness(path => new Promise(resolve => pending.push({path, resolve})));
  const first = c.refreshMeta(); await tick();
  c.state.nsfw = 'hide';
  assert.equal(c.refreshMeta(), first); assert.equal(c.refreshMeta(), first);
  for (const p of pending.splice(0)) p.resolve(value(p.path, 'old'));
  await tick();
  assert.equal(c.session.token, '');
  assert.equal(pending.length, 5);
  assert.ok(pending.find(p => p.path.startsWith('/api/identity/count')).path.includes('nsfw=hide'));
  for (const p of pending.splice(0)) p.resolve(value(p.path, 'fresh'));
  await first;
  assert.equal(c.session.token, 'fresh');
  assert.equal(c.state.chars[0].id, 'fresh');
});
test('tag-only refresh avoids characters, pools and counts', async () => {
  const calls = [];
  const c = metaHarness(async path => {calls.push(path); return ['tag'];});
  await c.refreshMeta({tagsOnly: true});
  assert.deepEqual(calls, ['/api/user-tags']);
  assert.equal(c.state.userTags[0], 'tag');
});
test('failed metadata refresh releases the in-flight request for retry', async () => {
  let fail = true;
  const c = metaHarness(async path => {if (fail) throw Error('offline'); return value(path);});
  await assert.rejects(c.refreshMeta(), /offline/);
  fail = false; await c.refreshMeta(); assert.equal(c.session.token, 'current');
});

class Element {
  constructor() {this.children = []; this.dataset = {}; this.style = {}; this.classList = {toggle() {}}; this.moves = 0;}
  get firstElementChild() {return this.children[0] || null;}
  get nextElementSibling() {return this.parent?.children[this.parent.children.indexOf(this) + 1] || null;}
  remove() {if (this.parent) {const a = this.parent.children; a.splice(a.indexOf(this), 1); this.parent = null;}}
  insertBefore(el, before) {if (el === before) return; el.remove(); const i = before ? this.children.indexOf(before) : this.children.length; assert.ok(i >= 0); this.children.splice(i, 0, el); el.parent = this; this.moves++;}
  replaceWith(el) {const p = this.parent; p.insertBefore(el, this); this.remove();}
  setAttribute() {}
  querySelector() {return {addEventListener() {}};}
}
function documentMock() {
  return {createElement(type) {
    if (type !== 'template') return new Element();
    return {set innerHTML(value) {this.content = {firstElementChild: Object.assign(new Element(), {html: value})};}};
  }};
}
// reconcileSections() is ui/dom.js (#199).
let reconcileSections;
test.before(async () => { ({reconcileSections} = await web.load('ui/dom.js')); });
test('section updates preserve unaffected nodes and support reorder/removal', () => {
  globalThis.document = documentMock(); const c = {reconcileSections};
  const root = new Element();
  c.reconcileSections(root, [['a','A'],['b','B'],['c','C']]);
  const [a,b,cc] = root.children; const before = root.moves;
  c.reconcileSections(root, [['a','changed'],['b','B'],['c','C']]);
  assert.notEqual(root.children[0], a); assert.equal(root.children[1], b); assert.equal(root.children[2], cc);
  assert.equal(root.moves - before, 1);
  c.reconcileSections(root, [['c','C'],['b','B']]);
  assert.deepEqual(root.children, [cc,b]);
});
// The grid is ui/grid.js, on a fake page; its timers and reads are the test's.
let grid;
test.before(async () => { grid = await web.load('ui/grid.js'); });
function gridPage({items = [], total = items.length, scrollTop = 0} = {}) {
  const page = web.fakePage(), {state} = web.core();
  Object.assign(page.el('viewport'), {scrollTop, clientHeight: 200, clientWidth: 202});
  Object.assign(state, {size: 100, fit: false, total, items, route: {page: 'library'}, dataVersion: 2e6 + Math.random(), gridKey: '',
    pages: new Set(), pending: new Set(), generation: 1, readEpoch: 0, countDirty: false, sel: {all: false, ids: new Set(), exclude: new Set()}});
  return {page, state};
}
const quietTimers = () => { const saved = [globalThis.setTimeout, globalThis.requestAnimationFrame]; globalThis.setTimeout = () => 0; return () => { [globalThis.setTimeout, globalThis.requestAnimationFrame] = saved; }; };
test('virtual grid keeps overlapping cards, updates changed items and relayouts columns', () => {
  const restore = quietTimers();
  try {
    const {page, state} = gridPage({items: Array.from({length: 30}, (_, i) => ({uuid: String(i), memberships: [], width: 100, height: 100}))});
    const el = page.el('grid'), viewport = page.el('viewport'), shown = top => grid.rowStart(Math.min(grid.rowCount(), state.fit ? grid.rowAt(top + 200) + 2 : Math.ceil((top + 200) / state.rowH) + 2)) - grid.rowStart(Math.max(0, grid.rowAt(top) - 2));
    grid.renderGrid(); const originals = [...el.children]; assert.equal(originals.length, shown(0));
    viewport.scrollTop = 100; grid.renderGrid();
    assert.equal(el.children.length, shown(100)); originals.forEach((card, i) => assert.equal(el.children[i], card));
    state.items[5] = {...state.items[5]}; state.dataVersion++; grid.renderGrid();
    assert.notEqual(el.children[5], originals[5]); assert.equal(el.children[4], originals[4]);
    viewport.scrollTop = 400; grid.renderGrid();
    const first = grid.rowStart(Math.max(0, grid.rowAt(400) - 2));
    assert.equal(Number(el.children[0].dataset.index), first); assert.equal(el.children[0], originals[first]);
    state.size = 60; grid.renderGrid();
    assert.equal(el.children[0].style.left, '0px');
  } finally { restore(); }
});
test('scroll bursts schedule a single render per frame', () => {
  const callbacks = [], saved = globalThis.requestAnimationFrame;
  globalThis.requestAnimationFrame = f => { callbacks.push(f); return callbacks.length; };
  try {
    const {page} = gridPage(); page.el('grid').hidden = true;
    for (let i = 0; i < 50; i++) grid.scheduleGrid();
    assert.equal(callbacks.length, 1); callbacks.shift()();
    grid.scheduleGrid(); assert.equal(callbacks.length, 1);
  } finally { globalThis.requestAnimationFrame = saved; }
});
test('pagination skips recount and obsolete completion cannot clear a newer pending page', async () => {
  let finish; const calls = [], restore = quietTimers();
  try {
    const {state} = gridPage({total: 200}); state.items = [];
    globalThis.fetch = path => { calls.push(path); return new Promise(resolve => finish = body => resolve({status: 200, ok: true, json: async () => body})); };
    const old = grid.loadPage(100);
    assert.match(calls[0], /offset=100/); assert.match(calls[0], /count=0/);
    state.generation++; state.pending = new Set([100]);
    finish({items: []}); await old;
    assert.ok(state.pending.has(100)); assert.equal(state.pages.size, 0);
    // A count that may have changed is read again with the first page, not with this one.
    state.pending.clear(); state.countDirty = true; const recount = grid.loadPage(100);
    await new Promise(r => setImmediate(r));
    assert.equal(calls.length, 2); assert.doesNotMatch(calls[1], /count=0/);
    finish({total: 200, items: []}); await recount;
  } finally { restore(); }
});

test('successful writes invalidate an in-flight metadata read before UI refresh starts', async () => {
  const pending = [];
  const c = metaHarness(path => path === '/api/tag' || path === '/api/suggest' ? {ok: true} : new Promise(resolve => pending.push({path,resolve})));
  c.state.readEpoch = 0;
  // Writes go through core/session.js; app.js counts them in its onPost listener.
  const {post, onPost, metaWritten} = web.core();
  Object.assign(c, {post, metaWritten, guideAfterPost() {}, onPost: fn => (c.unlisten = onPost(fn))});
  vm.createContext(c);
  vm.runInContext(section('onPost((path, body) =>', '// What a background job'), c);
  const first = c.refreshMeta(); await tick();
  await c.post('/api/suggest', {});
  assert.equal(c.state.readEpoch, 0); assert.equal(c.state.countDirty, undefined);
  await c.post('/api/tag', {});
  assert.equal(c.state.readEpoch, 1); assert.equal(c.state.countDirty, true);
  for (const p of pending.splice(0)) p.resolve(value(p.path, 'stale'));
  await tick(); assert.equal(c.session.token, '');
  for (const p of pending.splice(0)) p.resolve(value(p.path, 'after-write'));
  await first; assert.equal(c.session.token, 'after-write');
  c.unlisten();
});
test('reload counts only its first page and retries if data changed during the read', async () => {
  const pending = [], restore = quietTimers();
  try {
    const {state} = gridPage({total: 0}); state.items = [];
    globalThis.fetch = path => new Promise(resolve => pending.push({path, resolve: body => resolve({status: 200, ok: true, json: async () => body})}));
    const result = grid.reload(); state.readEpoch++;
    await new Promise(r => setImmediate(r));
    const first = pending.splice(0); assert.doesNotMatch(first[0].path, /count=0/); assert.ok(first.slice(1).every(p => /count=0/.test(p.path)));
    first.forEach((p, i) => p.resolve({total: i === 0 ? 200 : null, items: []})); await new Promise(r => setImmediate(r)); await new Promise(r => setImmediate(r));
    assert.notEqual(state.total, 200);   // data changed during the read: its answer is not used
    const again = pending.splice(0); assert.ok(again.length >= 1); assert.doesNotMatch(again[0].path, /count=0/);
    again.forEach((p, i) => p.resolve({total: i === 0 ? 201 : null, items: []})); await result;
    assert.equal(state.total, 201); assert.equal(state.countDirty, false);
  } finally { restore(); }
});
test('bootstrap seed is discarded when data or NSFW changes before the batch starts', async () => {
  for (const mutate of [c => c.state.readEpoch++, c => c.state.nsfw = 'hide']) {
    const calls = [];
    const c = metaHarness(async path => {calls.push(path); return value(path, 'fresh');});
    c.state.readEpoch = 0;
    const result = c.refreshMeta({boot:{token:'old',total:1}});
    mutate(c); await result;
    assert.equal(calls.filter(p => p.startsWith('/api/bootstrap')).length, 1);
    assert.equal(c.session.token, 'fresh');
  }
});

test('metadata requests summaries outside directory and full rows inside directory', async () => {
  const calls = [];
  const c = metaHarness(async path => { calls.push(path); return value(path); });
  await c.refreshMeta();
  assert.ok(calls.find(p => p.startsWith('/api/characters')).includes('summary=1'));
  calls.length = 0; c.state.route.page = 'characters';
  await c.refreshMeta();
  assert.ok(!calls.find(p => p.startsWith('/api/characters')).includes('summary=1'));
});
test('active character retains full variant counts rather than summary rows', async () => {
  const c = metaHarness(async path => path.startsWith('/api/characters?id=') ? [{id: 'person', variants: [{id: 'skin', count: 7}]}] : value(path, 'person'));
  c.state.char = {id: 'person'};
  await c.refreshMeta();
  assert.equal(c.state.char.variants[0].count, 7);
});
test('navigation into directory during summary read triggers a full trailing read', async () => {
  const pending = [];
  const c = metaHarness(path => new Promise(resolve => pending.push({path, resolve})));
  const task = c.refreshMeta(); await tick();
  assert.ok(pending.some(p => p.path.includes('summary=1')));
  c.state.route.page = 'characters';
  for (const p of pending.splice(0)) p.resolve(value(p.path));
  await tick();
  assert.equal(pending.length, 5);
  assert.ok(pending.every(p => !p.path.includes('summary=1')));
  for (const p of pending.splice(0)) p.resolve(value(p.path));
  await task;
});
test('group append, answer and selection preserve unaffected section nodes', () => {
  const sections = new Element(), controls = {}, more = {}, nodes = {regroup: {}, moreGroups: {addEventListener() {}}};
  const root = {querySelector: selector => ({'.gsecs': sections, '.groups-controls': controls, '.groups-more': more})[selector],
    set innerHTML(_) { throw Error('must not replace all groups'); }};
  globalThis.document = documentMock();
  const c = {document: globalThis.document, reconcileSections, $: id => id === 'groupsPage' ? root : nodes[id], number: String, esc: String,
    stamp: String, state: {pools: {unmatched: 100}}, groups: {list: [], shown: 2, off: new Map(), learner: {}},
    groupSection: g => `<section>${g.cid}:${JSON.stringify([...(c.groups.off.get(g.cid) || [])])}</section>`};
  vm.createContext(Object.assign(c, zh.page));
  vm.runInContext(section('function drawGroups()', 'function groupSection'), c);
  c.groups.list = [1, 2, 3].map(cid => ({cid, size: 2}));
  c.drawGroups(); const [a, b] = sections.children;
  c.groups.shown = 3; c.drawGroups(); const third = sections.children[2];
  assert.equal(sections.children[0], a); assert.equal(sections.children[1], b);
  c.groups.list.shift(); c.drawGroups();
  assert.deepEqual(sections.children, [b, third]);
  c.groups.off.set(2, new Set(['u'])); c.drawGroups();
  assert.notEqual(sections.children[0], b); assert.equal(sections.children[1], third);
});
test('older group reads cannot overwrite a newer R18-filtered response', async () => {
  const pending = [], nodes = {}, c = {state: {route: {page: 'groups'}, nsfw: ''}, groups: {revision: 0},
    $: id => nodes[id] ||= {}, closeInspector() {}, drawGroups() {},
    api: path => new Promise(resolve => pending.push({path, resolve}))};
  vm.createContext(Object.assign(c, zh.page)); vm.runInContext(section('async function renderGroups', 'function drawGroups'), c);
  const old = c.renderGroups();
  c.state.nsfw = 'hide'; const fresh = c.renderGroups();
  pending[1].resolve({clusters: [{cid: 2, size: 2}], learner: {}}); await fresh;
  pending[0].resolve({clusters: [{cid: 1, size: 2}], learner: {}}); await old;
  assert.equal(c.groups.list[0].cid, 2);
});

test('a library page finishing after navigation cannot overwrite home items', async () => {
  let resolve; const restore = quietTimers();
  try {
    const {state} = gridPage({total: 200}); state.items = [];
    globalThis.fetch = () => new Promise(r => resolve = body => r({status: 200, ok: true, json: async () => body}));
    const before = state.dataVersion, pending = grid.loadPage(100); state.route = {page: 'home'}; state.items = [{uuid: 'home'}];
    resolve({items: [{uuid: 'old-library'}]}); await pending;
    assert.equal(state.items.length, 1); assert.equal(state.items[0].uuid, 'home'); assert.equal(state.dataVersion, before);
  } finally { restore(); }
});
