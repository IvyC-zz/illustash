// 运行：node --test tests/poll.test.cjs
// 自己排下一次的轮询（core/poll.js 的 poller，#432 ③-5）：重叠时只有最新的一次采用回应、排下一次；读失败按 retry 再试。
// 定时器换成记录下来的假定时器，不真的等。
const {test, before, afterEach} = require('node:test');
const assert = require('node:assert/strict');
const web = require('./web.cjs');
let poll;
before(async () => { await web.ready(); poll = await web.load('core/poll.js'); });
const real = {set: globalThis.setTimeout, clear: globalThis.clearTimeout};
let timers;
function fakeTimers() {
  timers = new Map(); let id = 0;
  globalThis.setTimeout = (fn, ms) => { timers.set(++id, {fn, ms}); return id; };
  globalThis.clearTimeout = n => { timers.delete(n); };
}
afterEach(() => { globalThis.setTimeout = real.set; globalThis.clearTimeout = real.clear; });
const pending = () => [...timers.values()].map(x => x.ms);

test('an older answer that arrives last is dropped and schedules nothing', async () => {
  fakeTimers();
  const answers = [], adopted = [];
  const run = poll.poller(async fresh => {
    const value = await new Promise(resolve => answers.push(resolve));
    if (!fresh()) return 0;
    adopted.push(value); return 2000;
  });
  const first = run(), second = run();
  answers[1]('new'); await second;
  answers[0]('old'); await first;
  assert.deepEqual(adopted, ['new']);
  assert.deepEqual(pending(), [2000]);
});

test('a failed read retries after retry ms, and 0 stops polling', async () => {
  fakeTimers();
  let fail = true;
  const run = poll.poller(async () => { if (fail) throw new Error('合成失败'); return 0; }, {retry: 5000});
  await run();
  assert.deepEqual(pending(), [5000]);
  fail = false;
  const [{fn}] = timers.values(); await fn();
  assert.deepEqual(pending(), []);
});

test('later(ms) replaces whatever was scheduled', async () => {
  fakeTimers();
  const run = poll.poller(async () => 1500);
  await run();
  run.later(2000);
  assert.deepEqual(pending(), [2000]);
});
