const {test} = require('node:test');
const assert = require('node:assert/strict');
const {readFileSync} = require('node:fs');
const vm = require('node:vm');
const source = readFileSync(require('node:path').join(__dirname, '../web/static/app.js'), 'utf8');
const helpers = source.slice(source.indexOf('const settingsRequests ='), source.indexOf('// One page per question'));
function harness(api, extra = {}) {
  const renders = [], errors = [];
  const c = {api, state: {route: {page: 'settings'}}, document: {activeElement: null},
    error: message => errors.push(message), readError: e => '读取失败：' + e.message, renderSettings: fetch => renders.push(fetch), ...extra};
  vm.createContext(c); vm.runInContext(helpers, c);
  return {c, renders, errors};
}
const tick = () => new Promise(resolve => setImmediate(resolve));

test('cold load starts all four reads concurrently and shares in-flight requests', async () => {
  const pending = new Map(), calls = [];
  const {c, renders} = harness(path => { calls.push(path); return new Promise(resolve => pending.set(path, resolve)); });
  const load = c.loadSettingsData();
  assert.equal(c.loadSettingsData(), load);
  const same = c.settingsRequest('/api/names');
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
  vm.createContext(c);
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

function indexHarness(initialStatus, nextStatus) {
  const calls = [], renders = [];
  const c = {indexTimer: 0, INDEX_ACTIVE: ['checking','fetching','importing'],
    state: {route: {page:'settings'}, index: {state:{status:initialStatus}, works:[], history:[]}},
    api: async path => {calls.push(path); return {state:{status:nextStatus,done:2,total:10}};},
    settingsRequest: async path => {calls.push(path);return {state:{status:nextStatus},works:[],check:{found:1}};},
    clearTimeout() {}, setTimeout() {}, renderIndexCard() {renders.push('index');}, renderTasks() {renders.push('tasks');}, drawShelf() {},
    $: () => null, indexLine: () => '', toast() {}, refreshMeta: async () => {}, number: String};
  vm.createContext(c);
  vm.runInContext(source.slice(source.indexOf('async function pollIndex()'),source.indexOf('// ---------- display names')),c);
  return {c,calls,renders};
}

test('index progress does not reload work list or rebuild the editor', async () => {
  const {c,calls,renders} = indexHarness('checking','checking');
  await c.pollIndex();
  assert.deepEqual(calls,['/api/index?progress=1']);
  assert.deepEqual(renders,['tasks']);
});

test('index completion fetches updated results once', async () => {
  const {c,calls,renders} = indexHarness('checking','checked');
  await c.pollIndex();
  assert.deepEqual(calls,['/api/index?progress=1','/api/index']);
  assert.deepEqual(renders,['index','tasks']);
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
