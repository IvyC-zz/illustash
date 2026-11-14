// 运行：node --test tests/learning_settle.test.cjs
// 确认或撤销角色之后等后台学习收尾（core/meta.js 的 learningSettled；#400）：看到学习进行过、或学完的时间变了才算
// 落定；一直空闲、没有学习 worker、学习失败、读取失败或超时都不再读；更新的一次等待让之前的作废。
const {test, before, afterEach} = require('node:test');
const assert = require('node:assert/strict');
const web = require('./web.cjs');
before(web.ready);
let restore = () => {};
afterEach(() => { restore(); restore = () => {}; });

// states：每次读 /api/clusters 依次回答的 learner 状态（用完后重复最后一个）；字符串 'fail' 表示这次读取失败。
// 定时器立即触发，记下每次等了多久。
function harness(states) {
  const core = web.core(), waits = [], reads = [];
  core.state.nsfw = 'hide';
  const fetch = globalThis.fetch, setTimeout = globalThis.setTimeout;
  globalThis.fetch = async url => {
    assert.ok(url.startsWith('/api/clusters?'), url);
    const s = states[Math.min(reads.length, states.length - 1)]; reads.push(s);
    if (s === 'fail') throw new Error('offline');
    return {status: 200, ok: true, json: async () => ({clusters: [], learner: s})};
  };
  globalThis.setTimeout = (fn, delay) => { waits.push(delay); return setTimeout(fn, 0); };
  restore = () => { globalThis.fetch = fetch; globalThis.setTimeout = setTimeout; };
  return {settled: core.learningSettled, waits, reads, quiet: core.LEARN_QUIET, limit: core.LEARN_LIMIT};
}
const idle = at => at === undefined ? {status: 'idle'} : {status: 'idle', at};

test('learning that starts after the write and ends is settled', async () => {
  const {settled, reads} = harness([idle('10:00'), {status: 'learning'}, {status: 'importing'}, idle('10:01')]);
  assert.equal(await settled(idle('10:00')), true);
  assert.equal(reads.length, 4);
});

test('learning that already finished before the first read is caught by the changed time', async () => {
  const {settled, reads} = harness([idle('10:01')]);
  assert.equal(await settled(idle('10:00')), true);
  assert.equal(reads.length, 1);
});

test('a fresh session without a previous run counts its first finished run', async () => {
  const {settled} = harness([idle('10:01')]);
  assert.equal(await settled(idle()), true);
});

test('learning already running when the write was sent is settled at the next idle', async () => {
  const {settled} = harness([{status: 'waiting', message: '…'}, idle('10:05')]);
  assert.equal(await settled({status: 'learning'}), true);
});

test('staying idle means the write started no learning; stops after the quiet period', async () => {
  const {settled, waits, quiet} = harness([idle('10:00')]);
  assert.equal(await settled(idle('10:00')), false);
  const total = waits.reduce((a, b) => a + b, 0);
  assert.ok(total >= quiet && total - waits.at(-1) < quiet, `waited ${waits}`);
});

test('without the state before the write, the first idle read is the baseline', async () => {
  const {settled} = harness([idle('10:00'), {status: 'learning'}, idle('10:01')]);
  assert.equal(await settled(null), true);
  const {settled: again} = harness([idle('10:00')]);
  assert.equal(await again(null), false);
});

for (const [name, states, before_] of [
  ['no learning worker', [{status: 'off'}], idle('10:00')],
  ['learning failed', [{status: 'learning'}, {status: 'error', message: 'x'}], idle('10:00')],
  ['the read failed', ['fail'], idle('10:00')],
]) {
  test(`${name}: nothing to reread`, async () => {
    const {settled} = harness(states);
    assert.equal(await settled(before_), false);
  });
}

test('off before the write: no polling at all', async () => {
  const {settled, reads} = harness([idle('10:01')]);
  assert.equal(await settled({status: 'off'}), false);
  assert.equal(reads.length, 0);
});

test('learning that never ends gives up at the limit with backoff', async () => {
  const {settled, waits, limit} = harness([{status: 'learning'}]);
  assert.equal(await settled(idle('10:00')), false);
  const total = waits.reduce((a, b) => a + b, 0);
  assert.ok(total >= limit && total - waits.at(-1) < limit, `waited ${total}`);
  assert.equal(Math.max(...waits), 2000);
  assert.ok(waits.length < 40, `${waits.length} reads`);
});

test('a newer wait supersedes the older one', async () => {
  const {settled} = harness([idle('10:00'), {status: 'learning'}, idle('10:01')]);
  const first = settled(idle('10:00')), second = settled(idle('10:00'));
  assert.equal(await first, false);
  assert.equal(await second, true);
});
