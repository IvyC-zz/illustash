// Run with: node --test tests/name_network.test.cjs
const {test, before} = require('node:test');
const web = require('./web.cjs');   // core/ and ui/settings.js (#199)
let settings;
before(async () => { await web.ready(); settings = await web.load('ui/settings.js'); });
const assert = require('node:assert/strict');
// Lookups are core/settings.js (remoteNames, remoteNote); asking for consent is ui/settings.js setNamesNetwork(), with the
// page's own dialog (ui/dialog.js) and toast (ui/dom.js), here on a fake page.
function harness({fail, confirm = true} = {}) {
  const calls = [], toasts = [], dialogs = [], core = web.core(), nodes = {};
  core.state.names = {counts: {}}; core.state.route = {page: 'library'};
  globalThis.fetch = async (path, options) => {
    calls.push([path, options?.body]);
    if (fail) return {status: 400, ok: false, json: async () => ({error: fail})};
    return {status: 200, ok: true, json: async () => path === '/api/names' ? {network: {authorized: JSON.parse(options.body).allow}} : [{tag: 'synthetic'}]};
  };
  const element = () => ({hidden: true, set textContent(v) { if (this === nodes.toastText) toasts.push(v); }});
  globalThis.document = {getElementById: id => nodes[id] ||= element(), body: {append(d) {
    dialogs.push(d.querySelector('h2').textContent);
    setImmediate(() => d.close(confirm ? 'ok' : ''));   // the person answers
  }}, createElement() {
    const parts = {};
    return {returnValue: '', setAttribute() {}, querySelector: selector => parts[selector] ||= {}, showModal() {}, remove() {},
      close(value = '') { this.returnValue = value; this.onclose(); }};
  }};
  const setTimeout = globalThis.setTimeout; globalThis.setTimeout = () => 0;
  const context = {state: core.state, remoteNames: core.remoteNames, remoteNote: core.remoteNote,
    setNamesNetwork: async allow => { try { return await settings.setNamesNetwork(allow); } finally { globalThis.setTimeout = setTimeout; } }};
  return {context, calls, toasts, dialogs};
}

test('a refusal for want of consent is told apart from a real failure', async () => {
  let {context} = harness({fail: '显示名联网尚未授权'});
  let result = await context.remoteNames('/api/entities?q=x&remote=1');
  assert.equal(result.found, null);
  assert.equal(result.unauthorized, true);
  assert.equal(context.remoteNote(result), '查 Danbooru 需要先允许这个图库联网查询名称（只发送文字）');
  ({context} = harness({fail: '请求超时'}));
  result = await context.remoteNames('/api/entities?q=x&remote=1');
  assert.equal(result.unauthorized, false);
  assert.equal(context.remoteNote(result), 'Danbooru 暂时连不上：请求超时');
  ({context} = harness());
  assert.deepEqual((await context.remoteNames('/api/works?q=x&remote=1')).found, [{tag: 'synthetic'}]);
});

test('granting asks first and sends nothing when declined; revoking does not ask', async () => {
  let h = harness({confirm: false});
  assert.equal(await h.context.setNamesNetwork(true), false);
  assert.deepEqual(h.calls, []);
  assert.deepEqual(h.dialogs, ['允许联网查询名称']);
  h = harness();
  assert.equal(await h.context.setNamesNetwork(true), true);
  assert.deepEqual(h.calls, [['/api/names', '{"action":"network","allow":true}']]);
  assert.equal(h.context.state.names.network.authorized, true);
  assert.deepEqual(h.context.state.names.counts, {}, 'other name state is kept');
  h = harness();
  assert.equal(await h.context.setNamesNetwork(false), true);
  assert.deepEqual(h.dialogs, []);
  assert.deepEqual(h.calls, [['/api/names', '{"action":"network","allow":false}']]);
  assert.deepEqual(h.toasts, ['已撤销名称联网查询']);
});

test('saving or clearing model settings keeps the grant and revoke control available', async () => {
  // The model card is ui/settings.js; saving goes through core/settings.js saveModelApi().
  const {renderModelCard} = await web.load('ui/settings.js');
  for (const authorized of [true, false]) {
    const network = {authorized, this_run: false, granted_at: authorized ? 'synthetic-time' : null}, {state} = web.core();
    const controls = Object.fromEntries(['modelCard', 'modelSave', 'modelClear', 'modelTest', 'modelBase', 'modelName', 'modelKey', 'toast', 'toastText', 'toastAct', 'toastUndo']
      .map(id => [id, {value: '', contains: () => false}]));
    const calls = [];
    Object.assign(state, {route: {page: 'library'}, names: {network, model_api: {configured: true}}});
    globalThis.document = {getElementById: id => controls[id], activeElement: null};
    globalThis.fetch = async (path, options) => {
      const body = JSON.parse(options.body); calls.push([path, body]);
      return {status: 200, ok: true, json: async () => ({model_api: {configured: body.key !== null}, counts: {total: 2}})};
    };
    renderModelCard();
    await controls.modelSave.onclick();
    await new Promise(resolve => setImmediate(resolve));
    assert.equal(state.names.network, network);
    await controls.modelClear.onclick();
    await new Promise(resolve => setImmediate(resolve));
    assert.equal(state.names.network, network);
    assert.equal(state.names.model_api.configured, false);
    assert.equal(calls.length, 2);
    assert.ok(calls.every(([path, body]) => path === '/api/names' && body.action === 'model_api'));
  }
});

test('saving a new model address that drops the key says the key must be entered again', async () => {
  // The backend keeps a key only for the address it was entered for (app_names.rs configure).
  const {renderModelCard} = await web.load('ui/settings.js');
  for (const [kept, said] of [[true, '设置已保存'], [false, '接口地址已更改，原来的 API key 已清除，请重新填写']]) {
    const {state} = web.core();
    const controls = Object.fromEntries(['modelCard', 'modelSave', 'modelClear', 'modelTest', 'modelBase', 'modelName', 'modelKey', 'toast', 'toastText', 'toastAct', 'toastUndo']
      .map(id => [id, {value: '', contains: () => false}]));
    controls.modelBase.value = 'https://synthetic.invalid';
    Object.assign(state, {route: {page: 'library'}, names: {network: {}, model_api: {configured: true, base: 'https://api.deepseek.com'}}});
    globalThis.document = {getElementById: id => controls[id], activeElement: null};
    globalThis.fetch = async () => ({status: 200, ok: true, json: async () => ({model_api: {configured: kept}})});
    renderModelCard();
    controls.modelSave.onclick();
    await new Promise(resolve => setImmediate(resolve));
    assert.equal(controls.toastText.textContent, said);
  }
});

test('refreshing names from the task panel keeps the network status', async () => {
  // The panel's button runs core/jobs.js refreshNames().
  const network = {authorized: true, this_run: false, granted_at: 'synthetic-time'}, {state, refreshNames} = web.core();
  state.names = {network, state: {status: 'idle'}};
  globalThis.fetch = async (path, options) => {
    assert.equal(path, '/api/names'); assert.equal(JSON.parse(options.body).action, 'refresh');
    return {status: 200, ok: true, json: async () => ({state: {status: 'fetching'}})};
  };
  await refreshNames();
  assert.equal(state.names.network, network);
  assert.equal(state.names.state.status, 'fetching');
});

test('names waiting for consent are not shown as a failure', () => {
  const {namesLine} = web.core();
  const line = namesLine({status: 'unauthorized'});
  assert.doesNotMatch(line, /失败/);
  assert.match(line, /网络与隐私/);
  assert.equal(namesLine({status: 'error'}), '获取失败，将在下次启动时重试');
});
