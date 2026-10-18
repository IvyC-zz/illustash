// #34: answers that arrive after the page has moved on, or out of order, never wind the interface back,
// and a background refresh of the settings card keeps what is being typed.
const {test, before} = require('node:test');
const web = require('./web.cjs');   // core/ modules (#199)
before(web.ready);
const assert = require('node:assert/strict');
const {readFileSync} = require('node:fs');
const vm = require('node:vm');
const zh = require('./i18n_zh.cjs');   // t() of a Chinese page (#173)
const source = readFileSync(require('node:path').join(__dirname, '../web/static/app.js'), 'utf8');
const section = (a, b) => source.slice(source.indexOf(a), source.indexOf(b, source.indexOf(a)));
const tick = () => new Promise(resolve => setImmediate(resolve));
const element = () => ({hidden: false, classList: {remove() {}, toggle() {}}, listeners: {},
  addEventListener(kind, fn) { this.listeners[kind] = fn; }});

test('a group page answer that arrives after navigating home does not take over the page', async () => {
  const nodes = {}, painted = [];
  let answer, route = {page: 'group', id: '3'};
  const c = {
    state: {route: {page: 'home'}, nsfw: ''}, history: {replaceState() {}}, document: {querySelectorAll: () => []},
    $: id => nodes[id] ||= element(), parseHash: () => route, defaultSort: () => 'newest', hashFor: () => '',
    clearSelection() {}, closeInspector() {}, error: m => painted.push('error ' + m), readError: e => e.message,
    api: () => new Promise(resolve => { answer = resolve; }), nearText: () => 'near', fetchChar: async () => null,
    renderChrome: () => painted.push('chrome'), renderHome: () => painted.push('home'), reload: async () => painted.push('grid'),
    showGuideNav() {}, renderStart: () => painted.push('start'),
  };
  vm.createContext(Object.assign(c, zh.page));
  vm.runInContext(section('async function applyRoute()', "window.addEventListener('hashchange'"), c);
  const toGroup = c.applyRoute();
  await tick();
  route = {page: 'home'};
  await c.applyRoute();
  assert.equal(nodes.homePage.hidden, false);
  answer({clusters: [{cid: 3, near: 1, traits: ['old']}]});
  await toGroup;
  assert.equal(nodes.homePage.hidden, false);
  assert.equal(c.state.groupTraits, undefined);
  assert.deepEqual(painted, ['chrome', 'home']);
});

test('an older sync answer that arrives last does not move the state back', async () => {
  // core/jobs.js polls the sync; its timers are the page's setTimeout.
  const {state, pollSync} = web.core(), answers = [], timers = [];
  Object.assign(state, {sync: {status: 'scanning'}, route: {page: 'library'}});
  globalThis.fetch = () => new Promise(resolve => answers.push(body => resolve({status: 200, ok: true, json: async () => body})));
  const setTimeout = globalThis.setTimeout;
  globalThis.setTimeout = (fn, ms) => { timers.push(ms); return 0; };
  try {
    const older = pollSync(), newer = pollSync();
    answers[1]({state: {status: 'done', imported: 2}, history: [{id: 2}]});
    await newer;
    answers[0]({state: {status: 'scanning'}, history: [{id: 1}]});
    await older;
  } finally { globalThis.setTimeout = setTimeout; }
  assert.equal(state.sync.status, 'done');
  assert.deepEqual(state.syncHistory, [{id: 2}]);
  assert.deepEqual(timers, []);  // finished: nothing polls again, and the stale answer schedules nothing
});

test('the new-library name is kept as it is typed, so a card refresh restores it', () => {
  const nodes = {};
  const c = {$: id => nodes[id] ||= element()};
  vm.createContext(Object.assign(c, zh.page));
  vm.runInContext(section('const libForm =', '// The backend lists the origins') + ';this.libForm = libForm;', c);
  nodes.settingsPage.listeners.input({target: {id: 'libName', value: '合成新图库'}});
  nodes.settingsPage.listeners.input({target: {id: 'libPhotos', checked: true}});
  assert.equal(c.libForm.name, '合成新图库');
  assert.equal(c.libForm.photos, true);
});
