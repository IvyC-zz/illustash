// 用确定性的时钟和延迟的响应运行生产环境的轮询代码（core/settings.js）。
const {test, before, afterEach} = require('node:test');
const assert = require('node:assert/strict');
const web = require('./web.cjs');   // 登录等待在 core/settings.js 里（#199）
before(web.ready);
let restore = () => {};
afterEach(() => { restore(); restore = () => {}; });
function harness() {
  let now = 0, next = 0;
  const timers = new Map(), requests = [], messages = [];
  const core = web.core(), saved = {now: Date.now, setTimeout: globalThis.setTimeout, clearTimeout: globalThis.clearTimeout, fetch: globalThis.fetch};
  Date.now = () => now;
  globalThis.setTimeout = fn => { timers.set(++next, fn); return next; };
  globalThis.clearTimeout = id => { timers.delete(id); };
  globalThis.fetch = (path, options) => path === '/api/accounts' ? Promise.resolve({status: 200, ok: true, json: async () => ({})})
    : new Promise((resolve, reject) => requests.push({resolve: body => resolve({status: 200, ok: true, json: async () => body}), reject, signal: options.signal}));
  // 设置页显示的内容：已绑定的账号，或者等待为什么结束。
  const off = core.onSettings((what, detail) => { if (what === 'pixiv') messages.push(detail.error ? detail.error.message : 'bound'); });
  restore = () => { core.stopPixivLogin(); off(); Object.assign(globalThis, {setTimeout: saved.setTimeout, clearTimeout: saved.clearTimeout, fetch: saved.fetch}); Date.now = saved.now; };
  return {ctx: core, timers, requests, messages,
    elapse(ms) { now += ms; },
    tick() { const [id, fn] = timers.entries().next().value; timers.delete(id); return fn(); },
  };
}
test('slow responses cannot overlap, and success ends polling', async () => {
  const h = harness(); h.ctx.waitPixivLogin();
  const first = h.tick(); h.elapse(8000);
  assert.equal(h.timers.size, 0); assert.equal(h.requests.length, 1);
  h.requests[0].resolve({bound: false, login_open: true}); await first;
  assert.equal(h.timers.size, 1);
  const second = h.tick(); h.requests[1].resolve({bound: true, user_id: '123'}); await second;
  assert.equal(h.timers.size, 0); assert.equal(h.ctx.state.pixivWaiting, false);
  assert.equal(h.messages.length, 1);
});
for (const rejected of [false, true]) test(`superseded ${rejected ? 'error' : 'success'} is ignored`, async () => {
  const h = harness(); h.ctx.waitPixivLogin(); const old = h.tick();
  h.ctx.waitPixivLogin(); assert.equal(h.requests[0].signal.aborted, true);
  if (rejected) h.requests[0].reject(new Error('old error'));
  else h.requests[0].resolve({bound: true, user_id: 'old'});
  await old;
  assert.equal(h.messages.length, 0); assert.equal(h.ctx.state.pixivWaiting, true);
  assert.equal(h.timers.size, 1);
});
test('stopping invalidates an in-flight response', async () => {
  const h = harness(); h.ctx.waitPixivLogin(); const pending = h.tick();
  h.ctx.stopPixivLogin(); assert.equal(h.requests[0].signal.aborted, true);
  h.requests[0].resolve({bound: true, user_id: 'old'}); await pending;
  assert.equal(h.messages.length, 0); assert.equal(h.timers.size, 0);
  assert.equal(h.ctx.state.pixivWaiting, false);
});
test('closed window stops polling', async () => {
  const h = harness(); h.ctx.waitPixivLogin(); const pending = h.tick();
  h.requests[0].resolve({bound: false, login_open: false}); await pending;
  assert.equal(h.timers.size, 0); assert.equal(h.ctx.state.pixivWaiting, false);
});
test('expired deadline starts no new request', async () => {
  const h = harness(); h.ctx.waitPixivLogin(); h.elapse(600000); await h.tick();
  assert.equal(h.requests.length, 0); assert.equal(h.ctx.state.pixivWaiting, false);
});
test('current failure stops polling and reports the error', async () => {
  const h = harness(); h.ctx.waitPixivLogin(); const pending = h.tick();
  h.requests[0].reject(new Error('failed')); await pending;
  assert.deepEqual(h.messages, ['failed']); assert.equal(h.timers.size, 0);
  assert.equal(h.ctx.state.pixivWaiting, false);
});
