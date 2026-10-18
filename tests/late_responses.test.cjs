// #34: answers that arrive after the page has moved on, or out of order, never wind the interface back,
// and a background refresh of the settings card keeps what is being typed.
const {test, before} = require('node:test');
const web = require('./web.cjs');   // core/ modules (#199)
before(web.ready);
const assert = require('node:assert/strict');
const tick = () => new Promise(resolve => setImmediate(resolve));
const element = () => ({hidden: false, classList: {remove() {}, toggle() {}}, listeners: {},
  addEventListener(kind, fn) { this.listeners[kind] = fn; }});

test('a group page answer that arrives after navigating home does not take over the page', async () => {
  // applyRoute() is ui/shell.js; the home page is ui/home.js; the page is a fake one.
  const requests = [], {applyRoute} = await web.load('ui/shell.js'), {state} = web.core();
  let answer;
  Object.assign(state, {route: {page: 'home'}, nsfw: '', guide: undefined});
  globalThis.location = {hash: '#/g/3'}; globalThis.history = {replaceState() {}};
  globalThis.requestAnimationFrame ??= () => 0;
  const page = web.fakePage(), el = page.el;
  globalThis.fetch = path => {
    requests.push(path);
    if (path.startsWith('/api/clusters')) return new Promise(resolve => { answer = body => resolve({status: 200, ok: true, json: async () => body}); });
    return Promise.resolve({status: 200, ok: true, json: async () => path.startsWith('/api/home') ? {days: [], next: ''} : []});
  };
  const toGroup = applyRoute();
  await tick();
  location.hash = '#/home';
  await applyRoute();
  assert.equal(el('homePage').hidden, false);
  answer({clusters: [{cid: 3, near: 1, traits: ['old']}]});
  await toGroup;
  assert.equal(el('homePage').hidden, false);
  assert.equal(state.groupTraits, undefined);
  assert.match(el('heading').innerHTML, /首页/);
  assert.ok(requests.some(p => p.startsWith('/api/home')));
  assert.ok(!requests.some(p => p.startsWith('/api/assets')), 'the group page never reads its pictures');
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

test('the new-library name is kept as it is typed, so a card refresh restores it', async () => {
  const {onLibraryInput, libForm} = await web.load('ui/settings.js');
  onLibraryInput({target: {id: 'libName', value: '合成新图库'}});
  onLibraryInput({target: {id: 'libPhotos', checked: true}});
  assert.equal(libForm.name, '合成新图库');
  assert.equal(libForm.photos, true);
});
