const {test} = require('node:test');
const assert = require('node:assert/strict');
const vm = require('node:vm');
const zh = require('./i18n_zh.cjs');   // t() of a Chinese page (#173)
const source = require('node:fs').readFileSync(require('node:path').join(__dirname, '../web/static/app.js'), 'utf8');
const helper = source.slice(source.indexOf('async function checkIndex()'), source.indexOf('const NAMES_NOTE'));
function harness(confirmAction) {
  const calls = [];
  const c = {state: {token: 'library-a'}, confirmAction, post: async (path, body) => { calls.push({path, body}); return {state: {status: 'checking'}}; }};
  vm.createContext(Object.assign(c, zh.page)); vm.runInContext(helper, c);
  return {c, calls};
}
test('cancel sends no check; every accepted run requests consent again', async () => {
  let prompts = 0, allow = false;
  const {c, calls} = harness(async (text, title) => { prompts++; assert.match(text, /只发送关注作品的 tag/); assert.equal(title, '检查新角色'); return allow; });
  assert.equal(await c.checkIndex(), false); assert.equal(calls.length, 0);
  allow = true; await c.checkIndex(); await c.checkIndex();
  assert.equal(prompts, 3); assert.equal(calls.length, 2);
  for (const call of calls) { assert.equal(call.path, '/api/index'); assert.equal(call.body.action, 'check'); assert.equal(call.body.allow_network, true); }
});
test('a library change during confirmation cannot authorize the new library', async () => {
  let resolve;
  const {c, calls} = harness(() => new Promise(r => { resolve = r; }));
  const result = c.checkIndex(); c.state.token = 'library-b'; resolve(true);
  assert.equal(await result, false); assert.equal(calls.length, 0);
});
test('the task button is usable immediately after cancelled confirmation', async () => {
  const start = source.indexOf("$('taskPanel').addEventListener('click', async e => {");
  const code = source.slice(start, source.indexOf("document.addEventListener('click', e => { if (taskPanel.open", start));
  let click; const button = {dataset: {task: 'index'}, disabled: false};
  const c = {$: () => ({addEventListener: (_, handler) => { click = handler; }}), checkIndex: async () => false, taskTick: () => assert.fail('cancel starts no task'), toast: () => assert.fail('cancel is not an error')};
  vm.createContext(Object.assign(c, zh.page)); vm.runInContext(code, c);
  await click({stopPropagation() {}, target: {closest: () => button}});
  assert.equal(button.disabled, false);
});
