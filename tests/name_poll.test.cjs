// 运行：node --test tests/name_poll.test.cjs
// 轮询显示名任务（core/jobs.js 的 pollNames()；#199）：运行中只轮询进度，保留已有的覆盖率和设置；
// 结束时读一次这两项；轮询失败时保留已有内容并退避。
const {test, before, afterEach} = require('node:test');
const assert = require('node:assert/strict');
const web = require('./web.cjs');
let effects;
before(async () => { await web.ready(); effects = await web.load('ui/effects.js'); });
let restore = () => {};
afterEach(() => { restore(); restore = () => {}; });

function harness(status = 'running', fail = false) {
  const calls = [], timers = [], core = web.core();
  const names = {state: {status: 'running'}, counts: {characters: {total: 10, zh: 5}}, model_api: {configured: true}};
  Object.assign(core.state, {route: {page: 'settings'}, names});
  web.fakePage();
  globalThis.fetch = async url => {
    // 一次运行结束时刷新页面显示的名称：一次元数据读取（core/meta.js），记为一次调用。
    if (!url.startsWith('/api/names')) { if (url.startsWith('/api/bootstrap')) calls.push('refreshMeta'); return {status: 200, ok: true, json: async () => url.startsWith('/api/bootstrap') ? {token: '', total: 0} : []}; }
    calls.push(url);
    if (fail) throw new Error('offline');
    const body = url.includes('progress=1') ? {state: {status, done: 10, total: 10}} : {...names, state: {status}, counts: {characters: {total: 10, zh: 9}}};
    return {status: 200, ok: true, json: async () => body};
  };
  const setTimeout = globalThis.setTimeout;
  globalThis.setTimeout = (fn, delay) => { timers.push(delay); return 1; };
  // ui/effects.js 的 afterJob()，接法与 app.js 相同：重画卡片，结束时刷新页面显示的名称。
  const offs = [core.onJob(effects.afterJob)];
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
