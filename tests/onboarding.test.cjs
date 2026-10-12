// #96: the first-run guide of a new library. Which library opens on it, and what each step says from the state it reads.
const {test} = require('node:test');
const assert = require('node:assert/strict');
const {readFileSync} = require('node:fs');
const vm = require('node:vm');
const source = readFileSync(require('node:path').join(__dirname, '../web/static/app.js'), 'utf8');
const section = (a, b) => source.slice(source.indexOf(a), source.indexOf(b, source.indexOf(a)));
const element = () => ({hidden: false, innerHTML: '', dataset: {}, addEventListener() {}});

function guide({status = 'active', step = 'ability', capability = {}, results = {}, works, sync = {status: 'done', finished_at: '10-12 10:00'}, hash = '', index} = {}) {
  const nodes = {}, posted = [], replaced = [];
  const c = {
    state: {token: 't', route: {page: 'start'}, sync, index}, posted, replaced, nodes,
    location: {hash}, history: {replaceState: (a, b, url) => replaced.push(url)},
    $: id => nodes[id] ||= element(), esc: s => String(s ?? ''), number: n => String(n), toast() {},
    cardHead: (title, note = '') => `<h2>${title}</h2><p>${note}</p>`, shortcut: k => '⌘' + k,
    SYNC_ACTIVE: ['scanning', 'recognising', 'importing'], INDEX_ACTIVE: ['checking', 'fetching'],
    syncLine: s => 'line:' + s.status, syncBar: () => '', indexLine: () => 'index', WORK_NAMES: {genshin_impact: '原神'},
    closeInspector() {}, readFailure() {}, setTimeout: () => 0, clearTimeout() {},
    api: async (path, options) => {
      if (options?.method === 'POST') { const body = JSON.parse(options.body); posted.push(body); return {...c.reply, guide: {status: 'active', step: body.step || step}}; }
      return c.reply;
    },
    reply: {guide: {status, step}, capability: {recognition: true, index: true, anchors: 0, check: {status: 'ok'}, ...capability},
      results: {total: 0, recognized: 0, with_character: 0, unmatched: 0, characters: 0, ...results}, works},
  };
  vm.createContext(c);
  vm.runInContext(section('// ---------- 开始使用', '(async () => {'), c);
  return c;
}

test('a new library opens on the guide and records that it started', async () => {
  const c = guide({status: 'new'});
  await c.guideOnOpen();
  assert.deepEqual(c.posted, [{action: 'step', step: 'ability'}]);
  assert.deepEqual(c.replaced, ['#/start']);
});

test('an address the person opened is kept, and a library without a guide never opens it', async () => {
  const kept = guide({status: 'active', hash: '#/library'});
  await kept.guideOnOpen();
  assert.deepEqual([kept.posted, kept.replaced], [[], []]);
  for (const status of ['off', 'skipped', 'done']) {
    const c = guide({status});
    c.state.route = {page: 'home'};
    await c.guideOnOpen();
    assert.deepEqual([c.posted, c.replaced], [[], []], status);
    assert.equal(c.nodes.navStart.hidden, true, status);
  }
});

test('the first sync tells apart pictures still waiting from those recognised without a character', async () => {
  const c = guide({step: 'sync', results: {total: 10, recognized: 7, with_character: 4, unmatched: 6, characters: 3}});
  await c.renderStart();
  const html = c.nodes.startPage.innerHTML;
  assert.match(html, /3 张还没识别，下次同步会继续/);
  assert.match(html, /识别过但没认出角色的 3 张/);
  assert.match(html, /共 3 个角色/);
  assert.doesNotMatch(html, /先去看已经加入的图/);
});

test('without a recognition component the guide says so instead of checking', async () => {
  const c = guide({capability: {recognition: false, check: {status: 'idle'}}, results: {total: 2, unmatched: 2}});
  await c.renderStart();
  assert.match(c.nodes.startPage.innerHTML, /这个安装没有识别组件/);
  assert.deepEqual(c.posted, []);
});

test('suggested works show who is followed, who needs checking first, and who can be followed', async () => {
  const works = [{id: 'w1', tag: 'blue_archive', name: 'Blue Archive', count: 9, watchable: true, watched: true},
    {id: 'w2', tag: '', name: '未核对的作品', count: 4, watchable: false, watched: false},
    {id: null, tag: 'genshin_impact', name: 'Genshin Impact', count: 2, watchable: true, watched: false}];
  const c = guide({step: 'more', works, index: {watch: {works: ['w1']}, state: {}, check: null, mb_per_character: 1.1}});
  await c.renderStart();
  const html = c.nodes.startPage.innerHTML;
  assert.match(html, /Blue Archive.*已关注/s);
  assert.match(html, /需要先核对作品/);
  assert.match(html, /原神.*data-guide-watch="2">关注/s, 'a suggested tag shows its built-in Chinese name');
  assert.doesNotMatch(html, /data-guide="index-check" disabled/, 'a followed work enables the check');
});
