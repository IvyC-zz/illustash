// Run with: node --test tests/name_poll.test.cjs
const {test} = require('node:test');
const assert = require('node:assert/strict');
const {readFileSync} = require('node:fs');
const vm = require('node:vm');
const zh = require('./i18n_zh.cjs');   // t() of a Chinese page (#173)
const source = readFileSync(require('node:path').join(__dirname, '../web/static/app.js'), 'utf8');
const poll = source.slice(source.indexOf('async function pollNames()'), source.indexOf('// Collapsible blocks'));

function harness(status = 'running', fail = false) {
  const calls = [], timers = [];
  const names = {state: {status: 'running'}, counts: {characters: {total: 10, zh: 5}}, model_api: {configured: true}};
  const context = {
    state: {route: {page: 'settings'}, names}, namesTimer: 0,
    NAMES_ACTIVE: ['fetching', 'matching', 'translating', 'running'],
    async api(url) {
      calls.push(url);
      if (fail) throw new Error('offline');
      return url.includes('progress=1') ? {state: {status, done: 10, total: 10}}
        : {...names, state: {status}, counts: {characters: {total: 10, zh: 9}}};
    },
    clearTimeout() {}, setTimeout(fn, delay) { timers.push(delay); return 1; },
    renderNamesCard() {}, renderTasks() {}, refreshMeta() { calls.push('refreshMeta'); },
  };
  vm.createContext(Object.assign(context, zh.page));
  vm.runInContext(poll, context);
  return {context, calls, timers, names};
}

test('running polls only state and retains coverage and settings', async () => {
  const {context, calls, timers, names} = harness();
  await context.pollNames();
  assert.deepEqual(calls, ['/api/names?progress=1']);
  assert.equal(context.state.names.counts, names.counts);
  assert.equal(context.state.names.model_api, names.model_api);
  assert.equal(context.state.names.state.done, 10);
  assert.deepEqual(timers, [2000]);
});

for (const status of ['done', 'error']) {
  test(`${status} refreshes coverage once and stops polling`, async () => {
    const {context, calls, timers} = harness(status);
    await context.pollNames();
    assert.deepEqual(calls, ['/api/names?progress=1', '/api/names', 'refreshMeta']);
    assert.equal(context.state.names.counts.characters.zh, 9);
    assert.deepEqual(timers, []);
  });
}

test('failed poll preserves prior data and backs off', async () => {
  const {context, calls, timers, names} = harness('running', true);
  await context.pollNames();
  assert.deepEqual(calls, ['/api/names?progress=1']);
  assert.equal(context.state.names, names);
  assert.deepEqual(timers, [10000]);
});

test('a run keeps being polled away from settings (the task panel shows it)', async () => {
  const {context, calls, timers} = harness();
  context.state.route.page = 'library';
  await context.pollNames();
  assert.deepEqual(calls, ['/api/names?progress=1']);
  assert.deepEqual(timers, [2000]);
});
