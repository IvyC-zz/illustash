// 「找回画师」的轮询：运行中一次读取失败不能停止轮询；任务面板在任何页面都要看到进度，运行结束就停；进入设置页总是重新读取。
const {test} = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const source = fs.readFileSync(require('node:path').join(__dirname, '../web/static/app.js'), 'utf8');
const code = source.slice(source.indexOf('async function loadProvenance'), source.indexOf('function renderProvenanceCard'));
function harness(answers) {
  const timers = [], renders = [];
  const ctx = {
    state: {route: {page: 'settings'}, provenance: null}, provenanceTimer: 0, t: require('./i18n_zh.cjs'),
    setTimeout(fn, ms) { timers.push(ms); return timers.length; }, clearTimeout() {},
    renderProvenanceCard() { renders.push(ctx.state.provenance?.state?.status ?? null); }, renderTasks() {},
    api: async () => { const a = answers.shift(); if (a instanceof Error) throw a; return a; },
  };
  vm.createContext(ctx); vm.runInContext(code.replace('async function loadProvenance', 'var loadProvenance = async function'), ctx);
  return {ctx, timers, renders};
}
test('a failed read during a run keeps polling and keeps what was shown', async () => {
  const h = harness([{state: {status: 'running'}}, new Error('busy')]);
  await h.ctx.loadProvenance();
  assert.deepEqual(h.timers, [1500]);
  await h.ctx.loadProvenance();
  assert.deepEqual(h.timers, [1500, 4000]);
  assert.equal(h.ctx.state.provenance.state.status, 'running');
});
test('a run is polled from any page and polling stops once it is over', async () => {
  const h = harness([{state: {status: 'running'}}, {state: {status: 'done'}}]);
  h.ctx.state.route.page = 'library';
  await h.ctx.loadProvenance();
  assert.deepEqual(h.timers, [1500]);
  await h.ctx.loadProvenance();
  assert.deepEqual(h.timers, [1500]);
});
test('entering the settings page always reads again', () => {
  assert.match(source, /if \(\$\('setProvenance'\)\.innerHTML\) renderProvenanceCard\(\); loadProvenance\(\);/);
});
