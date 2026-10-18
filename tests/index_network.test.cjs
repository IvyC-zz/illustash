// Checking for new characters reaches Danbooru: it asks first, every time, and a library switched while the question is
// open is never checked (core/jobs.js checkIndex(); the task panel's button is ui/tasks.js; #199).
const {test, before} = require('node:test');
const assert = require('node:assert/strict');
const web = require('./web.cjs');
before(web.ready);

function harness() {
  const calls = [], core = web.core();
  core.session.token = 'library-a';
  globalThis.fetch = async (path, options) => { calls.push({path, body: JSON.parse(options.body)}); return {status: 200, ok: true, json: async () => ({state: {status: 'checking'}})}; };
  return {core, calls};
}
test('cancel sends no check; every accepted run requests consent again', async () => {
  let prompts = 0, allow = false;
  const {core, calls} = harness();
  const confirm = async (text, title) => { prompts++; assert.match(text, /只发送关注作品的 tag/); assert.equal(title, '检查新角色'); return allow; };
  assert.equal(await core.checkIndex(confirm), false); assert.equal(calls.length, 0);
  allow = true; await core.checkIndex(confirm); await core.checkIndex(confirm);
  assert.equal(prompts, 3); assert.equal(calls.length, 2);
  for (const call of calls) { assert.equal(call.path, '/api/index'); assert.equal(call.body.action, 'check'); assert.equal(call.body.allow_network, true); }
  assert.equal(core.state.index.state.status, 'checking');
});
test('a library change during confirmation cannot authorize the new library', async () => {
  let resolve;
  const {core, calls} = harness();
  const result = core.checkIndex(() => new Promise(r => { resolve = r; })); core.session.token = 'library-b'; resolve(true);
  assert.equal(await result, false); assert.equal(calls.length, 0);
});
test('the task button is usable immediately after cancelled confirmation', async () => {
  const {calls} = harness(), {document, el} = web.fakePage();
  // The page's own dialog (ui/dialog.js), cancelled as soon as it shows.
  let asked = 0;
  document.body.append = d => { asked++; d.showModal = () => { d.returnValue = 'cancel'; d.onclose(); }; };
  const {wireTasks} = await web.load('ui/tasks.js');
  wireTasks();
  const button = {dataset: {task: 'index'}, disabled: false};
  for (const click of el('taskPanel').listeners.click) await click({stopPropagation() {}, target: {closest: selector => selector === '[data-task]' ? button : null}});
  assert.equal(asked, 1);
  assert.equal(button.disabled, false);
  assert.deepEqual(calls, [], 'cancel sends nothing and starts no task');
});
