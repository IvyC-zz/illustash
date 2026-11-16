// 运行：node --test tests/wd_extension_panels.test.cjs
// WD 角色扩展（#550）在设置页和任务面板里的位置：「设置 › 角色资料」的卡片（范围标「本图库」）、读取失败时的提示，
// 以及任务面板里下载、启用、撤回共用的一项。各状态的文字和按钮见 tests/wd_extension.test.cjs。
const {test, before} = require('node:test');
const assert = require('node:assert/strict');
const web = require('./web.cjs');
let page, settings, tasks, state;
before(async () => {
  await web.ready();
  page = web.fakePage();
  globalThis.window = {addEventListener() {}};
  globalThis.requestAnimationFrame = () => 0;
  const store = new Map();
  globalThis.localStorage = {getItem: k => store.has(k) ? store.get(k) : null, setItem: (k, v) => store.set(k, v), removeItem: k => store.delete(k)};
  settings = await web.load('ui/settings.js');
  tasks = await web.load('ui/tasks.js');
  state = web.core().state;
});
const READY = {package: {version: 'wd-extension-v1', heads: 228, file: {path: 'wd-extension-v1.bin', size: 940701, sha256: 'ab'.repeat(32)}, license: 'Apache-2.0',
  status: 'ready', have: 940701, location: '/synthetic/Shared/extensions/wd-extension-v1'}, download: null,
  library: {enabled: false, images: 0, assigned: 0, blocked: null, vectors: 12}, task: null};

test('卡片在「角色资料」里，范围标「本图库」，状态和按钮来自 ui/wd_extension.js', async () => {
  state.route = {page: 'settings', section: 'characters'};
  state.wdExtension = READY;
  await settings.renderWdExtensionCard();
  const html = page.el('wdExtCard').innerHTML;
  assert.match(html, /^<h2>WD 角色扩展<span class="set-scope" data-scope="library" title="每个图库各自保存">本图库<\/span><\/h2>/);
  assert.match(html, /<p class="settings-note">多认 228 个 WD 词表里没有的较新角色。<\/p>/);
  assert.match(html, /本图库未启用/);
  assert.match(html, /data-wdx="enable-confirm">启用…/);
  assert.equal(settings.settingsSection()[2], '新角色检查、WD 角色扩展、角色名称与翻译');
  assert.deepEqual(settings.settingsSection()[3], ['indexCard', 'wdExtCard', 'namesCard', 'modelCard']);
});

test('还没读到时读一次；读取失败写在卡片里', async () => {
  state.route = {page: 'settings', section: 'characters'};
  state.wdExtension = null;
  const calls = [];
  globalThis.fetch = async path => { calls.push(path); return {status: 404, ok: false, headers: {get: () => null}, json: async () => ({error: '没有这个接口'})}; };
  await settings.renderWdExtensionCard();
  assert.deepEqual(calls, ['/api/wd-extension']);
  assert.match(page.el('wdExtCard').innerHTML, /读取失败：没有这个接口/);
});

test('任务面板：有下载或任务时才列出，失败时打警告', () => {
  state.wdExtension = READY;
  assert.equal(tasks.taskList().find(x => x.key === 'wdExtension'), undefined);
  state.wdExtension = {...READY, task: {id: 't', kind: 'enable', status: 'running', phase: 'scan', done: 3, total: 12}};
  const running = tasks.taskList().find(x => x.key === 'wdExtension');
  assert.equal(running.name, 'WD 角色扩展');
  assert.equal(running.active, true);
  assert.equal(running.line, '正在检查 3 / 12 张');
  assert.match(running.action, /data-wdx="cancel"/);
  state.wdExtension = {...READY, task: {id: 't', kind: 'withdraw', status: 'failed', message: '磁盘已满', diagnostic: {id: 'D-1', stored: true}}};
  const failed = tasks.taskList().find(x => x.key === 'wdExtension');
  assert.equal(failed.warn, true);
  assert.match(failed.line, /^撤回没有完成，图库没有改动：磁盘已满 · /);
});
