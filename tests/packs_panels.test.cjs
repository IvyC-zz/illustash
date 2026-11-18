// 运行：node --test tests/packs_panels.test.cjs
// 角色资料包（#602）在设置页和任务面板里的位置：「设置 › 角色资料」的一张卡片（范围标「本图库」，原来 WD 角色扩展和 CCIP
// 长尾包的两张合成一张）、读取失败时的提示，以及任务面板里下载、启用、撤回共用的一项。各状态的文字和按钮见 tests/packs.test.cjs。
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
const part = (path, heads) => ({format: 'x', heads, candidate_only: 0, license: 'MIT', file: {path, size: 1000, sha256: 'ab'.repeat(32)}});
const READY = {catalog: {configured: true, serial: null, checked_at: null, error: null},
  current: {version: '0.1', stage: 'preview', revision: 1, heads: 282, candidate_only: 0, license: 'Apache-2.0 AND MIT', size: 2214393,
    status: 'ready', have: 2214393, components: {wd: part('wd-extension-v1.bin', 228), ccip: part('A-longtail-p75.zip', 54)}},
  offer: null, download: null, task: null,
  library: {enabled: false, version: null, wd: false, ccip: false, prompt: 'declined', images: 0, assigned: 0, vectors: 12, checked_at: null, blocked: null},
  location: '/synthetic/Shared/extensions', wd: {package: {}}, ccip: {library: {}}};

test('一张卡片在「角色资料」里，范围标「本图库」，状态和按钮来自 ui/packs.js', async () => {
  state.route = {page: 'settings', section: 'characters'};
  state.pack = READY;
  await settings.renderPackCard();
  const html = page.el('packCard').innerHTML;
  assert.match(html, /^<h2>角色资料包<span class="set-scope" data-scope="library" title="每个图库各自保存">本图库<\/span><\/h2>/);
  assert.match(html, /<p class="settings-note">多认 282 个 WD 词表和角色索引里没有的角色。<\/p>/);
  assert.match(html, /本图库未启用/);
  assert.match(html, /data-pack="enable-confirm">启用…/);
  assert.equal(settings.settingsSection()[2], '角色资料包、新角色检查、角色名称与翻译');
  assert.deepEqual(settings.settingsSection()[3], ['packCard', 'indexCard', 'namesCard', 'modelCard']);
});

test('等着默认启用（#630）的卡片底部照样写还缺几个角色（#606）', async () => {
  state.route = {page: 'settings', section: 'characters'};
  state.pack = null;
  const index = {state: {status: 'idle'}, works: [], watch: {works: ['work_a'], min_posts: 50}, check: {checked_at: 't', missing: []},
    history: [], mb_per_character: 2};
  const coverage = {checked_at: 't', coverage: {work_a: {packs: 3, own: 0, missing: 2}}};
  globalThis.fetch = async path => ({status: 200, ok: true, headers: {get: () => null},
    json: async () => path === '/api/index/coverage' ? coverage : index});
  await settings.renderIndexCard();
  await new Promise(resolve => setImmediate(resolve));
  state.pack = {...READY, current: {...READY.current, status: 'missing', have: 0},
    download: {kind: 'download', status: 'failed', message: '连接失败'},
    library: {...READY.library, prompt: 'pending', vectors: 12}};
  await settings.renderPackCard();
  const html = page.el('packCard').innerHTML;
  assert.match(html, /角色资料包没能下载：连接失败/);
  assert.match(html, /下载完成后启用/);
  assert.match(html, /data-pack="download">重试/);
  assert.doesNotMatch(html, /data-pack="decline"/);
  assert.match(html, /关注的作品还缺 2 个角色/);
  assert.match(html, /data-index-go/);
  // 包到了、等后台任务结束时同样。
  state.pack = {...READY, library: {...READY.library, prompt: 'pending', vectors: 12}};
  await settings.renderPackCard();
  assert.match(page.el('packCard').innerHTML, /等后台任务结束后启用[\s\S]*关注的作品还缺 2 个角色/);
});

test('还没读到时读一次；读取失败写在卡片里', async () => {
  state.route = {page: 'settings', section: 'characters'};
  state.pack = null;
  const calls = [];
  globalThis.fetch = async path => { calls.push(path); return {status: 404, ok: false, headers: {get: () => null}, json: async () => ({error: '没有这个接口'})}; };
  await settings.renderPackCard();
  assert.deepEqual(calls, ['/api/packs']);
  assert.match(page.el('packCard').innerHTML, /读取失败：没有这个接口/);
});

test('任务面板：有下载或任务时才列出，失败时打警告', () => {
  state.pack = READY;
  assert.equal(tasks.taskList().find(x => x.key === 'pack'), undefined);
  state.pack = {...READY, task: {id: 't', kind: 'enable', status: 'running', phase: 'scan', done: 3, total: 12}};
  const running = tasks.taskList().find(x => x.key === 'pack');
  assert.equal(running.name, '角色资料包');
  assert.equal(running.active, true);
  assert.equal(running.line, '正在检查 3 / 12 张');
  assert.match(running.action, /data-pack="cancel"/);
  state.pack = {...READY, task: {id: 't', kind: 'withdraw', status: 'failed', message: '磁盘已满', diagnostic: {id: 'D-1', stored: true}}};
  const failed = tasks.taskList().find(x => x.key === 'pack');
  assert.equal(failed.warn, true);
  assert.match(failed.line, /^撤回没有完成，图库没有改动：磁盘已满 · /);
});
