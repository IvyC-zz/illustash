const {test, before, afterEach} = require('node:test');
const web = require('./web.cjs');   // core/settings.js, ui/settings.js (#199)
const assert = require('node:assert/strict');
const {readFileSync} = require('node:fs');
const vm = require('node:vm');
const zh = require('./i18n_zh.cjs');   // t() of a Chinese page (#173)
const source = readFileSync(require('node:path').join(__dirname, '../web/static/app.js'), 'utf8');
let ui, off = () => {};
before(async () => { await web.ready(); ui = await web.load('ui/settings.js'); });
afterEach(() => { off(); off = () => {}; });
// A settings page that has read nothing yet; api answers its reads. What the page does on each part that arrives is
// ui/settings.js's onSettings listener: an error goes to the banner, and the settings page redraws (renderSettings(false)).
function harness(api) {
  const renders = [], errors = [], core = web.core(), {state} = core;
  for (const key of ['settings', 'fold', 'index', 'names']) delete state[key];
  state.route = {page: 'settings'};
  globalThis.fetch = path => { const answer = api(path); return Promise.resolve({status: 200, ok: true, json: () => answer}); };
  globalThis.document = {activeElement: null};
  off = core.onSettings((what, detail) => {
    if (what !== 'data') return;
    if (detail.error) errors.push(core.readError(detail.error));
    if (state.route.page === 'settings') renders.push(false);
  });
  const c = {state, loadSettingsData: core.loadSettingsData, shared: core.shared, setSettingsHTML: ui.setSettingsHTML, document: globalThis.document};
  return {c, renders, errors};
}
const tick = () => new Promise(resolve => setImmediate(resolve));

test('cold load starts all four reads concurrently and shares in-flight requests', async () => {
  const pending = new Map(), calls = [];
  const {c, renders} = harness(path => { calls.push(path); return new Promise(resolve => pending.set(path, resolve)); });
  const load = c.loadSettingsData();
  assert.equal(c.loadSettingsData(), load);
  const same = c.shared('/api/names');
  assert.deepEqual(calls, ['/api/settings','/api/fold','/api/index','/api/names']);
  pending.get('/api/fold')({kinds: ['duplicate']}); await tick();
  assert.deepEqual(c.state.fold, ['duplicate']);
  assert.deepEqual(renders, [false]); // useful partial data paints before the slow reads complete
  for (const path of ['/api/settings','/api/index','/api/names']) pending.get(path)({loaded: true});
  await Promise.all([load, same]);
  assert.equal(renders.length, 4);
  c.loadSettingsData();
  assert.equal(calls.length, 4); // warm rendering never refetches
});

test('a failed card does not block others and remains retryable', async () => {
  let failed = true;
  const {c, errors} = harness(async path => {
    if (failed && path === '/api/names') throw new Error('offline');
    return path === '/api/fold' ? {kinds: []} : {loaded: true};
  });
  await c.loadSettingsData();
  assert.equal(c.state.names, undefined);
  assert.deepEqual(errors, ['读取失败：offline']);
  assert.equal(c.state.settings.loaded, true);
  failed = false; await c.loadSettingsData();
  assert.equal(c.state.names.loaded, true);
});

test('responses after navigating away update cached data without repainting', async () => {
  const {c, renders} = harness(async () => ({kinds: []}));
  const load = c.loadSettingsData(); c.state.route.page = 'library'; await load;
  assert.equal(renders.length, 0);
});

test('unchanged card and active form edits do not replace DOM', () => {
  const {c} = harness(async () => ({})); let writes = 0;
  const card = {contains: el => !!el, set innerHTML(value) { writes++; }};
  assert.equal(c.setSettingsHTML(card, 'one'), true);
  assert.equal(c.setSettingsHTML(card, 'one'), false);
  c.document.activeElement = {matches: () => true};
  assert.equal(c.setSettingsHTML(card, 'two'), false);
  assert.equal(writes, 1);
  c.document.activeElement = null;
  assert.equal(c.setSettingsHTML(card, 'two'), true);
  assert.equal(writes, 2);
});

// 资料库 · 作品 (#198): the work tree replaces the settings page's work table.
function shelfHarness(works, watch = []) {
  const root = {className: '', innerHTML: '', querySelectorAll: () => []};
  const c = {state: {route: {page: 'characters', section: 'works'}, chars: [{work_id: 'a'}, {work_id: 'a'}, {work_id: 'b'}],
      index: {works, watch: {works: watch, pending_tags: []}, check: {missing: [{copyright: 'tag_a'}]}}},
    $: () => root, esc: String, number: String, prefs: {get: () => ({})}, watchDraft: '', WORK_NAMES: {},
    wireWatchAdd() {}, wireFolds() {}, adoptWatch() {}};
  vm.createContext(Object.assign(c, zh.page));
  const start = source.indexOf('const shelf = {');
  vm.runInContext(source.slice(start, source.indexOf('async function saveWatch', start)), c);
  return {c, root};
}
test('work tree lists children under their parent, with counts and the watch switch', () => {
  const works = [{id: 'b', name: 'child', parent_id: 'a', count: 1, watchable: false},
    {id: 'a', name: 'parent', parent_id: null, count: 9, tag: 'tag_a', watchable: true},
    {id: 'c', name: 'orphan', parent_id: 'gone', count: 3, status: 'pending', watchable: false}];
  const {c, root} = shelfHarness(works, ['a']);
  c.drawShelf();
  const html = root.innerHTML, order = ['parent', 'child', 'orphan'].map(n => html.indexOf(`<b>${n}</b>`));
  assert.deepEqual([...order].sort((x, y) => x - y), order);
  assert.match(html, /style="--depth:1"[^]*<b>child<\/b>/);
  assert.match(html, /data-watch="a" checked/);
  assert.match(html, /data-watch="b"\s+disabled/);
  assert.match(html, /\+1 个新角色/);
  assert.match(html, /链接待确认/);
  vm.runInContext("shelf.filter = 'watched'", c); c.drawShelf();
  assert.ok(root.innerHTML.includes('<b>parent</b>') && !root.innerHTML.includes('<b>child</b>'));
});

// The index's poller is core/jobs.js; what it redraws outside the task panel is app.js's onJob listener.
async function indexHarness(initialStatus, nextStatus) {
  const calls = [], renders = [], {state, pollIndex, onJob, SYNC_ACTIVE} = web.core();
  Object.assign(state, {route: {page: 'settings'}, index: {state: {status: initialStatus}, works: [], history: []}});
  globalThis.fetch = async path => {
    calls.push(path);
    const body = path.includes('progress=1') ? {state: {status: nextStatus, done: 2, total: 10}} : {state: {status: nextStatus}, works: [], check: {found: 1}};
    return {status: 200, ok: true, json: async () => body};
  };
  const g = {state, SYNC_ACTIVE, onJob: fn => offs.push(onJob(fn)), renderIndexCard() { renders.push('index'); }, drawShelf() {}, refreshMeta: async () => {}, error() {}, readError: e => e};
  const offs = [];
  vm.createContext(g);
  vm.runInContext(source.slice(source.indexOf('// What a background job'), source.indexOf('// Name text queries')), g);
  offs.push(onJob(() => { renders.push('tasks'); }));   // as the task panel does
  const setTimeout = globalThis.setTimeout;
  globalThis.setTimeout = () => 0;
  try { await pollIndex(); } finally { globalThis.setTimeout = setTimeout; for (const off of offs) off(); }
  return {calls, renders};
}

test('index progress does not reload work list or rebuild the editor', async () => {
  const {calls, renders} = await indexHarness('checking', 'checking');
  assert.deepEqual(calls, ['/api/index?progress=1']);
  assert.deepEqual(renders, ['tasks']);
});

test('index completion fetches updated results once', async () => {
  const {calls, renders} = await indexHarness('checking', 'checked');
  assert.deepEqual(calls, ['/api/index?progress=1', '/api/index']);
  assert.deepEqual(renders, ['index', 'tasks']);
});

test('new work data invalidates a collapsed editor even when its summary is unchanged', () => {
  const {c} = harness(async () => ({})); let writes = 0;
  const card = {contains: () => false, set innerHTML(value) {writes++;}};
  const works = [{id:'1',name:'old'}];
  c.setSettingsHTML(card, 'same summary', works);
  c.setSettingsHTML(card, 'same summary', works);
  assert.equal(writes, 1);
  c.setSettingsHTML(card, 'same summary', [{id:'1',name:'new'}]);
  assert.equal(writes, 2);
});

test('returning to settings refreshes cached data once without clearing it', async () => {
  const calls = [];
  const {c, errors} = harness(async path => {calls.push(path);return path === '/api/fold' ? {kinds: []} : {updated:true};});
  Object.assign(c.state,{settings:{old:true},fold:[],index:{old:true},names:{old:true}});
  const load = c.loadSettingsData(true);
  assert.equal(c.state.settings.old,true);
  assert.equal(c.loadSettingsData(true),load);
  await load;
  assert.equal(calls.length,4);
  assert.equal(c.state.names.updated,true);
});

test('the names and new-characters cards read their data when they open', async () => {
  const {state} = web.core(), nodes = {};
  state.route = {page: 'settings'};
  const card = () => ({hidden: true, innerHTML: '', contains: () => false, querySelectorAll: () => []});
  globalThis.document = {getElementById: id => nodes[id] ||= card(), activeElement: null};
  globalThis.fetch = async path => ({status: 200, ok: true, json: async () => path === '/api/names'
    ? {state: {status: 'idle'}, counts: {characters: {total: 2, zh: 1}}}
    : {state: {status: 'idle'}, works: [], watch: {works: [], min_posts: 50}, history: [], mb_per_character: 2}});
  await ui.renderNamesCard();
  await ui.renderIndexCard();
  assert.match(nodes.namesCard.innerHTML, /角色名称/);
  assert.match(nodes.indexCard.innerHTML, /检查新角色/);
});
