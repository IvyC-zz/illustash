// Run with: node --test tests/name_network.test.cjs
const {test} = require('node:test');
const assert = require('node:assert/strict');
const {readFileSync} = require('node:fs');
const vm = require('node:vm');
const source = readFileSync(require('node:path').join(__dirname, '../web/static/app.js'), 'utf8');
const code = source.slice(source.indexOf('const NAMES_UNAUTHORIZED'), source.indexOf('// 设置 › 网络与隐私'));

function harness({fail, confirm = true} = {}) {
  const calls = [], toasts = [], dialogs = [];
  const context = {
    state: {names: {counts: {}}},
    async api(path, options) {
      calls.push([path, options?.body]);
      if (fail) throw new Error(fail);
      return path === '/api/names' ? {network: {authorized: JSON.parse(options.body).allow}} : [{tag: 'synthetic'}];
    },
    async confirmAction(message, title) { dialogs.push(title); return confirm; },
    toast(m) { toasts.push(m); },
    document: {addEventListener() {}},
  };
  vm.createContext(context);
  vm.runInContext(code + ';this.remoteNames = remoteNames; this.remoteNote = remoteNote; this.setNamesNetwork = setNamesNetwork;', context);
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
  for (const authorized of [true, false]) {
    const network = {authorized, this_run: false, granted_at: authorized ? 'synthetic-time' : null};
    const controls = Object.fromEntries(['modelCard', 'modelSave', 'modelClear', 'modelTest', 'modelBase', 'modelName', 'modelKey']
      .map(id => [id, {value: ''}]));
    const calls = [], rendered = [];
    const context = {
      state: {names: {network, model_api: {configured: true}}},
      $(id) { return controls[id]; },
      esc(value) { return value; }, cardHead() { return ''; }, setSettingsHTML() { return true; },
      async post(path, body) { calls.push([path, body]); return {model_api: {configured: body.key !== null}, counts: {total: 2}}; },
      toast() {}, renderSettings() { rendered.push(context.state.names.network); },
    };
    vm.createContext(context);
    vm.runInContext(source.slice(source.indexOf('function renderModelCard()'), source.indexOf('async function pollNames()'))
      + ';renderModelCard();', context);
    controls.modelSave.onclick();
    await new Promise(resolve => setImmediate(resolve));
    await controls.modelClear.onclick();
    assert.deepEqual(rendered, [network, network]);
    assert.equal(context.state.names.network, network);
    assert.equal(context.state.names.model_api.configured, false);
    assert.equal(calls.length, 2);
    assert.ok(calls.every(([path, body]) => path === '/api/names' && body.action === 'model_api'));
  }
});

test('refreshing names from the task panel keeps the network status', async () => {
  const network = {authorized: true, this_run: false, granted_at: 'synthetic-time'};
  let handler;
  const context = {
    state: {names: {network, state: {status: 'idle'}}},
    $() { return {addEventListener(event, callback) { handler = callback; }}; },
    async post(path, body) {
      assert.equal(path, '/api/names'); assert.equal(body.action, 'refresh');
      return {state: {status: 'fetching'}};
    },
    toast() {}, taskTick() {},
  };
  vm.createContext(context);
  vm.runInContext(source.slice(source.lastIndexOf("$('taskPanel').addEventListener('click'"),
    source.indexOf("document.addEventListener('click', e => { if (taskPanel.open")), context);
  await handler({
    stopPropagation() {},
    target: {closest() { return {dataset: {task: 'names'}}; }},
  });
  assert.equal(context.state.names.network, network);
  assert.equal(context.state.names.state.status, 'fetching');
});

test('names waiting for consent are not shown as a failure', () => {
  const context = {number: String};
  vm.createContext(context);
  const start = source.indexOf('const NAMES_ACTIVE');
  vm.runInContext(source.slice(start, source.indexOf('\n', source.indexOf('const namesLine', start))) + ';this.namesLine = namesLine;', context);
  const line = context.namesLine({status: 'unauthorized'});
  assert.doesNotMatch(line, /失败/);
  assert.match(line, /网络与隐私/);
  assert.equal(context.namesLine({status: 'error'}), '获取失败，将在下次启动时重试');
});
