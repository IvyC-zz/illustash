const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const zh = require('./i18n_zh.cjs');   // t() of a Chinese page (#173)
const source = fs.readFileSync('web/static/app.js', 'utf8');
const tick = () => new Promise(resolve => setImmediate(resolve));
const section = (a, b) => source.slice(source.indexOf(a), source.indexOf(b, source.indexOf(a)));
function metaHarness(api) {
  const nodes = {}, paints = [];
  const c = {URLSearchParams, api, state: {nsfw: '', chars: [], dupInfo: {}, exclInfo: {}, route: {page: 'library'}},
    $: id => nodes[id] ||= {}, setUndo() {}, navCount() {}, fetchChar: async () => null,
    renderChrome: () => paints.push('chrome'), renderSettings() {}, renderIndexBody() {}, guideAfterPost() {},
    document: {createElement: () => ({})}};
  vm.createContext(Object.assign(c, {t: zh}));
  vm.runInContext(section('const SESSION_EXPIRED', 'async function api('), c);
  vm.runInContext(section('let metaPending', '// ---------- chrome'), c);
  return c;
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
  assert.equal(c.state.token, 'seed');
});
test('in-flight changes trigger one trailing refresh and stale responses never apply', async () => {
  const pending = [];
  const c = metaHarness(path => new Promise(resolve => pending.push({path, resolve})));
  const first = c.refreshMeta(); await tick();
  c.state.nsfw = 'hide';
  assert.equal(c.refreshMeta(), first); assert.equal(c.refreshMeta(), first);
  for (const p of pending.splice(0)) p.resolve(value(p.path, 'old'));
  await tick();
  assert.equal(c.state.token, undefined);
  assert.equal(pending.length, 5);
  assert.ok(pending.find(p => p.path.startsWith('/api/identity/count')).path.includes('nsfw=hide'));
  for (const p of pending.splice(0)) p.resolve(value(p.path, 'fresh'));
  await first;
  assert.equal(c.state.token, 'fresh');
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
  fail = false; await c.refreshMeta(); assert.equal(c.state.token, 'current');
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
test('section updates preserve unaffected nodes and support reorder/removal', () => {
  const c = {document: documentMock()}; vm.createContext(Object.assign(c, {t: zh}));
  vm.runInContext(section('function reconcileSections', 'function renderIndexBody'), c);
  const root = new Element();
  c.reconcileSections(root, [['a','A'],['b','B'],['c','C']]);
  const [a,b,cc] = root.children; const before = root.moves;
  c.reconcileSections(root, [['a','changed'],['b','B'],['c','C']]);
  assert.notEqual(root.children[0], a); assert.equal(root.children[1], b); assert.equal(root.children[2], cc);
  assert.equal(root.moves - before, 1);
  c.reconcileSections(root, [['c','C'],['b','B']]);
  assert.deepEqual(root.children, [cc,b]);
});
test('virtual grid keeps overlapping cards, updates changed items and relayouts columns', () => {
  const grid = new Element(), viewport = {scrollTop: 0, clientHeight: 200};
  const c = {document: documentMock(), $: id => id === 'grid' ? grid : viewport,
    state: {cols: 2, cellW: 100, rowH: 100, gap: 0, total: 30, fit: false, route: {page: 'library'}, dataVersion: 0,
      items: Array.from({length: 30}, (_, i) => ({uuid: String(i), memberships: []}))},
    layout() {}, cardHTML: item => item.uuid, isSquare: () => true, syncCards() {}, clearTimeout() {}, setTimeout() {}, pageTimer: null, PAGE: 100};
  vm.createContext(Object.assign(c, {t: zh})); vm.runInContext(section('let gridLayoutKey', 'const captionH ='), c);
  vm.runInContext(section('function renderGrid', 'function syncCards'), c);
  c.renderGrid(); const originals = [...grid.children]; assert.equal(originals.length, 8);
  viewport.scrollTop = 100; c.renderGrid();
  assert.equal(grid.children.length, 10); originals.forEach((el,i) => assert.equal(grid.children[i], el));
  c.state.items[5] = {...c.state.items[5]}; c.state.dataVersion++; c.renderGrid();
  assert.notEqual(grid.children[5], originals[5]); assert.equal(grid.children[4], originals[4]);
  viewport.scrollTop = 400; c.renderGrid();
  assert.equal(grid.children[0].dataset.index, 4); assert.equal(grid.children[0], originals[4]);
  c.state.cols = 3; c.renderGrid();
  assert.equal(grid.children[0].style.left, '0px');
});
test('scroll bursts schedule a single render per frame', () => {
  const callbacks = [], renders = [];
  const c = {requestAnimationFrame: f => {callbacks.push(f); return callbacks.length;}, renderGrid: () => renders.push(1),
    $: () => ({addEventListener() {}}), ResizeObserver: class {observe() {}}};
  vm.createContext(Object.assign(c, {t: zh})); vm.runInContext(section('let gridFrame', '// ---------- zoom'), c);
  for (let i = 0; i < 50; i++) c.scheduleGrid();
  assert.equal(callbacks.length, 1); callbacks.shift()(); assert.equal(renders.length, 1);
  c.scheduleGrid(); assert.equal(callbacks.length, 1);
});
test('pagination skips recount and obsolete completion cannot clear a newer pending page', async () => {
  let finish; const calls = [];
  const c = {state: {pages: new Set(), pending: new Set(), total: 200, generation: 1, readEpoch: 0, items: []},
    pageUrl: (offset, count) => {assert.equal(count, false); return String(offset);},
    api: path => {calls.push(path); return new Promise(resolve => finish = resolve);},
    abortList: null, renderGrid() {}, error() {}, reload: async () => calls.push('reload')};
  vm.createContext(Object.assign(c, {t: zh})); vm.runInContext(section('async function loadPage', 'function updateCount'), c);
  const old = c.loadPage(100);
  c.state.generation++; c.state.pending = new Set([100]);
  finish({items: []}); await old;
  assert.ok(c.state.pending.has(100)); assert.equal(c.state.pages.size, 0);
  c.state.pending.clear(); c.state.countDirty = true; await c.loadPage(100);
  assert.deepEqual(calls, ['100','reload']);
});

test('successful writes invalidate an in-flight metadata read before UI refresh starts', async () => {
  const pending = [];
  const c = metaHarness((path, options) => options?.method === 'POST' ? Promise.resolve({ok:true}) : new Promise(resolve => pending.push({path,resolve})));
  c.state.readEpoch = 0;
  vm.runInContext(section('async function post(', 'function error('), c);
  const first = c.refreshMeta(); await tick();
  await c.post('/api/suggest', {});
  assert.equal(c.state.readEpoch, 0); assert.equal(c.state.countDirty, undefined);
  await c.post('/api/tag', {});
  assert.equal(c.state.readEpoch, 1); assert.equal(c.state.countDirty, true);
  for (const p of pending.splice(0)) p.resolve(value(p.path, 'stale'));
  await tick(); assert.equal(c.state.token, undefined);
  for (const p of pending.splice(0)) p.resolve(value(p.path, 'after-write'));
  await first; assert.equal(c.state.token, 'after-write');
});
test('reload counts only its first page and retries if data changed during the read', async () => {
  const nodes = {}, pending = [], counts = [];
  const c = {AbortController, state: {route: {page:'library'}, generation:0, readEpoch:0, pending:new Set()},
    abortList:null, $:id=>nodes[id] ||= {}, panelPage:()=>false, layout(){}, error(){}, visibleOffsets:()=>[0,100],
    pageUrl:(offset,count)=>{counts.push(count);return offset;}, api:()=>new Promise(resolve=>pending.push(resolve)),
    renderGrid(){}, updateCount(){}, renderSelection(){}, renderHeading(){}};
  vm.createContext(Object.assign(c, {t: zh}));vm.runInContext(section('async function reload(', 'async function loadPage('),c);
  const result=c.reload(); c.state.readEpoch++;
  pending.splice(0).forEach((resolve,i)=>resolve({total:i===0?200:null,items:[]})); await tick();
  assert.equal(c.state.total,undefined);
  pending.splice(0).forEach((resolve,i)=>resolve({total:i===0?201:null,items:[]}));await result;
  assert.deepEqual(counts,[true,false,true,false]);assert.equal(c.state.total,201);assert.equal(c.state.countDirty,false);
});
test('bootstrap seed is discarded when data or NSFW changes before the batch starts', async () => {
  for (const mutate of [c => c.state.readEpoch++, c => c.state.nsfw = 'hide']) {
    const calls = [];
    const c = metaHarness(async path => {calls.push(path); return value(path, 'fresh');});
    c.state.readEpoch = 0;
    const result = c.refreshMeta({boot:{token:'old',total:1}});
    mutate(c); await result;
    assert.equal(calls.filter(p => p.startsWith('/api/bootstrap')).length, 1);
    assert.equal(c.state.token, 'fresh');
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
  const c = metaHarness(async path => value(path, 'person'));
  c.state.char = {id: 'person'};
  c.fetchChar = async id => ({id, variants: [{id: 'skin', count: 7}]});
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
  const c = {document: documentMock(), $: id => id === 'groupsPage' ? root : nodes[id], number: String, esc: String,
    stamp: String, state: {pools: {unmatched: 100}}, groups: {list: [], shown: 2, off: new Map(), learner: {}},
    groupSection: g => `<section>${g.cid}:${JSON.stringify([...(c.groups.off.get(g.cid) || [])])}</section>`};
  vm.createContext(Object.assign(c, {t: zh}));
  vm.runInContext(section('function reconcileSections', '// Kept for callers'), c);
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
  vm.createContext(Object.assign(c, {t: zh})); vm.runInContext(section('async function renderGroups', 'function drawGroups'), c);
  const old = c.renderGroups();
  c.state.nsfw = 'hide'; const fresh = c.renderGroups();
  pending[1].resolve({clusters: [{cid: 2, size: 2}], learner: {}}); await fresh;
  pending[0].resolve({clusters: [{cid: 1, size: 2}], learner: {}}); await old;
  assert.equal(c.groups.list[0].cid, 2);
});

test('a library page finishing after navigation cannot overwrite home items', async () => {
  let resolve, paints = 0;
  const c = {state:{route:{page:'library'},pages:new Set(),pending:new Set(),total:200,generation:1,readEpoch:0,items:[]},
    pageUrl:()=>'/api/assets',api:()=>new Promise(r=>resolve=r),abortList:null,renderGrid(){paints++;},error(){throw Error('stale error');}};
  vm.createContext(Object.assign(c, {t: zh}));vm.runInContext(section('async function loadPage', 'function updateCount'),c);
  const pending=c.loadPage(100);c.state.route={page:'home'};c.state.items=[{uuid:'home'}];
  resolve({items:[{uuid:'old-library'}]});await pending;
  assert.equal(c.state.items.length,1);assert.equal(c.state.items[0].uuid,'home');assert.equal(paints,0);
});
