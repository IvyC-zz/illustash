// Run with: node --test tests/name_poll.test.cjs
// Polling the display-name job (core/jobs.js pollNames(); #199): a run polls only its progress and keeps the coverage
// and settings it has; its end reads them once; a failed poll keeps what it has and backs off.
const {test, before, afterEach} = require('node:test');
const assert = require('node:assert/strict');
const vm = require('node:vm');
const web = require('./web.cjs');
const source = require('node:fs').readFileSync(require('node:path').join(__dirname, '../web/static/app.js'), 'utf8');
before(web.ready);
let restore = () => {};
afterEach(() => { restore(); restore = () => {}; });

function harness(status = 'running', fail = false) {
  const calls = [], timers = [], core = web.core();
  const names = {state: {status: 'running'}, counts: {characters: {total: 10, zh: 5}}, model_api: {configured: true}};
  Object.assign(core.state, {route: {page: 'settings'}, names});
  globalThis.fetch = async url => {
    calls.push(url);
    if (fail) throw new Error('offline');
    const body = url.includes('progress=1') ? {state: {status, done: 10, total: 10}} : {...names, state: {status}, counts: {characters: {total: 10, zh: 9}}};
    return {status: 200, ok: true, json: async () => body};
  };
  const setTimeout = globalThis.setTimeout;
  globalThis.setTimeout = (fn, delay) => { timers.push(delay); return 1; };
  // app.js's onJob listener: the card is redrawn, and the end refreshes what names the page shows.
  const offs = [], g = {state: core.state, onJob: fn => offs.push(core.onJob(fn)), renderNamesCard() {}, refreshMeta() { calls.push('refreshMeta'); }};
  vm.createContext(g);
  vm.runInContext(source.slice(source.indexOf('// What a background job'), source.indexOf('// A picture the app cannot decode')), g);
  restore = () => { globalThis.setTimeout = setTimeout; for (const off of offs) off(); };
  return {state: core.state, pollNames: core.pollNames, calls, timers, names};
}

test('running polls only state and retains coverage and settings', async () => {
  const {state, pollNames, calls, timers, names} = harness();
  await pollNames();
  assert.deepEqual(calls, ['/api/names?progress=1']);
  assert.equal(state.names.counts, names.counts);
  assert.equal(state.names.model_api, names.model_api);
  assert.equal(state.names.state.done, 10);
  assert.deepEqual(timers, [2000]);
});

for (const status of ['done', 'error']) {
  test(`${status} refreshes coverage once and stops polling`, async () => {
    const {state, pollNames, calls, timers} = harness(status);
    await pollNames();
    assert.deepEqual(calls, ['/api/names?progress=1', '/api/names', 'refreshMeta']);
    assert.equal(state.names.counts.characters.zh, 9);
    assert.deepEqual(timers, []);
  });
}

test('failed poll preserves prior data and backs off', async () => {
  const {state, pollNames, calls, timers, names} = harness('running', true);
  await pollNames();
  assert.deepEqual(calls, ['/api/names?progress=1']);
  assert.equal(state.names, names);
  assert.deepEqual(timers, [10000]);
});

test('a run keeps being polled away from settings (the task panel shows it)', async () => {
  const {state, pollNames, calls, timers} = harness();
  state.route.page = 'library';
  await pollNames();
  assert.deepEqual(calls, ['/api/names?progress=1']);
  assert.deepEqual(timers, [2000]);
});
