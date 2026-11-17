// 运行：node --test tests/learning_settle.test.cjs
// 确认或撤销角色之后等后台学习落定（core/meta.js 的 learningSettled；#400、#404）：按 GET /api/learning 回答的状态序列
// 验证。第一次不带 upto 读，以后带上第一次回答的 upto；done、sync、error、stopped 就停，读取失败按 error，等满时限按
// timeout；更新的一次等待让之前的作废；等待期间按需标记「正在更新」（state.learning）。
const {test, before, afterEach} = require('node:test');
const assert = require('node:assert/strict');
const web = require('./web.cjs');
before(web.ready);
let restore = () => {};
afterEach(() => { restore(); restore = () => {}; });

// states：每次读 /api/learning 依次回答的 state（用完后重复最后一个）；'fail' 表示这次读取失败。
// 定时器立即触发，记下每次等了多久；marks 记下每次读之前 state.learning 的值。
function harness(states, upto = 42) {
  const core = web.core(), waits = [], urls = [], marks = [];
  core.state.learning = false;
  const fetch = globalThis.fetch, setTimeout = globalThis.setTimeout;
  globalThis.fetch = async url => {
    assert.ok(url.startsWith('/api/learning'), url);
    const s = states[Math.min(urls.length, states.length - 1)]; urls.push(url); marks.push(core.state.learning);
    if (s === 'fail') throw new Error('offline');
    return {status: 200, ok: true, json: async () => ({upto, state: s, learner: {status: 'idle'}})};
  };
  globalThis.setTimeout = (fn, delay) => { waits.push(delay); return setTimeout(fn, 0); };
  restore = () => { globalThis.fetch = fetch; globalThis.setTimeout = setTimeout; };
  return {core, waits, urls, marks, limit: core.LEARN_LIMIT};
}
const first = (state, upto = 42) => ({upto, state, learner: {status: 'idle'}});

test('the first read goes without upto, later reads carry the upto it returned', async () => {
  const {core, urls} = harness(['pending', 'pending', 'done'], 7);
  const now = await core.learningNow();
  assert.deepEqual(now, first('pending', 7));
  assert.equal(await core.learningSettled(now), 'done');
  assert.deepEqual(urls, ['/api/learning', '/api/learning?upto=7', '/api/learning?upto=7']);
});

test('done on the first read: no polling', async () => {
  const {core, urls} = harness(['done']);
  assert.equal(await core.learningSettled(first('done')), 'done');
  assert.equal(urls.length, 0);
});

for (const [state, outcome] of [['error', 'error'], ['stopped', 'error'], ['sync', 'sync']]) {
  test(`${state} on the first read resolves to ${outcome} without polling`, async () => {
    const {core, urls} = harness(['done']);
    assert.equal(await core.learningSettled(first(state)), outcome);
    assert.equal(urls.length, 0);
  });
  test(`${state} while waiting resolves to ${outcome}`, async () => {
    const {core} = harness(['pending', state]);
    assert.equal(await core.learningSettled(first('pending')), outcome);
  });
}

test('a failed read or no state at all counts as error', async () => {
  const {core} = harness(['fail']);
  assert.equal(await core.learningSettled(first('pending')), 'error');
  assert.equal(await core.learningSettled(null), 'error');
});

test('learningNow resolves to null when the read fails', async () => {
  const {core} = harness(['fail']);
  assert.equal(await core.learningNow(), null);
});

test('polls every 250 ms for the first 2 s, then every second, up to the limit', async () => {
  const {core, waits, limit} = harness(['pending']);
  assert.equal(await core.learningSettled(first('pending')), 'timeout');
  const total = waits.reduce((a, b) => a + b, 0);
  assert.equal(total, limit);
  assert.deepEqual(waits.slice(0, 9), [250, 250, 250, 250, 250, 250, 250, 250, 1000]);
  assert.ok(waits.slice(8).every(w => w === 1000), `waited ${waits}`);
});

test('marking shows the updating flag while pending and clears it once settled', async () => {
  const {core, marks} = harness(['pending', 'pending', 'done']);
  const settled = core.learningSettled(first('pending'), {marking: true});
  // 第一次等之前就标上，调用方随即重画。
  assert.equal(core.state.learning, true);
  assert.equal(await settled, 'done');
  assert.deepEqual(marks, [true, true, true]);
  assert.equal(core.state.learning, false);
});

test('without marking the flag stays off', async () => {
  const {core, marks} = harness(['pending', 'done']);
  assert.equal(await core.learningSettled(first('pending')), 'done');
  assert.deepEqual(marks, [false, false]);
});

test('timeout clears the flag', async () => {
  const {core} = harness(['pending']);
  assert.equal(await core.learningSettled(first('pending'), {marking: true}), 'timeout');
  assert.equal(core.state.learning, false);
});

test('a newer wait supersedes the older one and takes over the flag', async () => {
  const {core} = harness(['pending', 'pending', 'done']);
  const older = core.learningSettled(first('pending'), {marking: true});
  assert.equal(core.state.learning, true);
  // 确认角色的等待不标记：接手时把撤销留下的标记去掉。
  const newer = core.learningSettled(first('pending'));
  assert.equal(core.state.learning, false);
  assert.equal(await older, 'superseded');
  assert.equal(await newer, 'done');
});

test('a wait cut short by sync resumes with the same upto after the sync ends', async () => {
  const {core, urls} = harness(['sync', 'pending', 'done'], 9);
  assert.equal(core.afterSyncLearning(), null);
  assert.equal(await core.learningSettled(first('pending', 9), {marking: true}), 'sync');
  // 同步进行中不标记、不再轮询。
  assert.equal(core.state.learning, false);
  const resumed = core.afterSyncLearning();
  assert.equal(core.state.learning, true);
  assert.equal(await resumed, 'done');
  assert.deepEqual(urls, ['/api/learning?upto=9', '/api/learning?upto=9', '/api/learning?upto=9']);
  // 接着等只做一次。
  assert.equal(core.afterSyncLearning(), null);
});

test('sync on the first read is remembered for after the sync', async () => {
  const {core} = harness(['done'], 3);
  assert.equal(await core.learningSettled(first('sync', 3)), 'sync');
  assert.equal(await core.afterSyncLearning(), 'done');
});

test('a newer wait forgets the sync wait it replaces', async () => {
  const {core} = harness(['done']);
  assert.equal(await core.learningSettled(first('sync')), 'sync');
  assert.equal(await core.learningSettled(first('done')), 'done');
  assert.equal(core.afterSyncLearning(), null);
});
