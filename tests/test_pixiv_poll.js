// Runs the production polling code with a deterministic clock and delayed responses.
const {test} = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const zh = require('./i18n_zh.cjs');   // t() of a Chinese page (#173)
const source = fs.readFileSync(require('node:path').join(__dirname, '../web/static/app.js'), 'utf8');
const code = source.slice(source.indexOf('let pixivPoll = null;'), source.indexOf('async function accountAction'));
function harness() {
  let now = 0, next = 0;
  const timers = new Map(), requests = [], messages = [];
  const ctx = {
    state: {}, AbortController, Date: {now: () => now}, t: zh,
    setTimeout(fn) { timers.set(++next, fn); return next; },
    clearTimeout(id) { timers.delete(id); },
    renderAccountsCard() {}, loadAccounts() {}, toast: text => messages.push(text),
    api(path, options) {
      return new Promise((resolve, reject) => requests.push({resolve, reject, signal: options.signal}));
    },
  };
  vm.createContext(ctx); vm.runInContext(code, ctx);
  return {ctx, timers, requests, messages,
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
