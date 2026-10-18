// #96: the first-run guide, taught on the real interface. Which library starts it, what the checklist ticks after each
// change, and which question is asked when.
const {test} = require('node:test');
const assert = require('node:assert/strict');
const {readFileSync} = require('node:fs');
const vm = require('node:vm');
const zh = require('./i18n_zh.cjs');   // t() of a Chinese page (#173)
const source = readFileSync(require('node:path').join(__dirname, '../web/static/app.js'), 'utf8');
const section = (a, b) => source.slice(source.indexOf(a), source.indexOf(b, source.indexOf(a)));
const element = () => ({hidden: false, innerHTML: '', dataset: {}, style: {}, offsetWidth: 300, offsetHeight: 120,
  classList: {add() {}, remove() {}, toggle() {}}, addEventListener() {}, querySelector: () => element(), querySelectorAll: () => []});
const plain = v => JSON.parse(JSON.stringify(v));

function guide({status = 'active', step = 'welcome', ticked = [], seen = [], hidden = false, results = {}, works, names, sync = {status: 'done'}, route = 'home', index} = {}) {
  const nodes = {}, posted = [];
  const c = {
    state: {token: 't', route: {page: route}, sync, names, index}, posted, nodes,
    $: id => nodes[id] ||= element(), esc: s => String(s ?? ''), number: n => String(n), toast() {}, shortcut: k => '⌘' + k,
    SYNC_ACTIVE: ['scanning', 'recognising', 'importing'], syncLine: s => 'line:' + s.status, WORK_NAMES: {blue_archive: '蔚蓝档案'},
    setTimeout: () => 0, requestAnimationFrame() {}, addEventListener() {}, innerWidth: 1200, innerHeight: 800, go() {},
    document: {querySelector: () => null, getElementById: () => null, addEventListener() {}},
    post: async (path, body) => { posted.push([path, body]); return {id: 'w1', watch: {works: ['w1']}}; },
    api: async (path, options) => {
      if (options?.method === 'POST') { const body = JSON.parse(options.body); posted.push(['/api/onboarding', body]); return c.reply; }
      return c.reply;
    },
    reply: {guide: {status, step, ticked, seen, hidden}, capability: {recognition: true, index: true, anchors: 0, check: {status: 'ok'}},
      results: {total: 0, recognized: 0, with_character: 0, unmatched: 0, characters: 0, ...results}, works},
  };
  vm.createContext(Object.assign(c, {t: zh}));
  vm.runInContext(section('// ---------- 新图库的首次引导', '(async () => {'), c);
  return c;
}

test('a new library records that its guide started; a library without one stays quiet', async () => {
  const fresh = guide({status: 'new'});
  await fresh.guideOnOpen();
  assert.deepEqual(plain(fresh.posted), [['/api/onboarding', {action: 'step', step: 'welcome'}]]);
  const old = guide({status: 'off'});
  await old.guideOnOpen();
  assert.deepEqual(plain(old.posted), []);
  old.drawGuide();
  assert.equal(old.nodes.checklist.hidden, true);
  assert.equal(old.nodes.guideNotice.hidden, true);
});

test('each kind of change ticks its checklist item once', async () => {
  const c = guide({route: 'groups'});
  await c.loadGuide();
  c.guideAfterPost('/api/change', {action: 'confirmed', ids: ['a']});
  c.state.route = {page: 'library'};
  c.guideAfterPost('/api/change', {action: 'rejected', ids: ['a']});
  c.guideAfterPost('/api/undo', {id: 3});
  c.guideAfterPost('/api/undo', {id: 4});
  c.guideAfterPost('/api/index', {action: 'watch', works: []});
  c.guideAfterPost('/api/index', {action: 'watch', works: ['w1']});
  assert.deepEqual(plain(c.state.guide.guide.ticked), ['group', 'correct', 'undo', 'follow']);
  assert.deepEqual(plain(c.posted.filter(([, b]) => b.action === 'tick').map(([, b]) => b.item)), ['group', 'correct', 'undo', 'follow']);
  c.drawChecklist();
  assert.match(c.nodes.checklist.innerHTML, /4\/5/);
});

test('names are asked once characters show, and not again once answered', async () => {
  const names = {network: {authorized: false}};
  const c = guide({results: {characters: 3, with_character: 4}, names, sync: {status: 'recognising'}});
  await c.loadGuide();
  c.drawNudge();
  assert.match(c.nodes.nudge.innerHTML, /认出了 3 个角色。要联网查它们的中文名吗/);
  const answered = guide({results: {characters: 3}, names, seen: ['names']});
  await answered.loadGuide();
  answered.drawNudge();
  assert.equal(answered.nodes.nudge.innerHTML, '');
});

test('after the first sync the most-pictured followable work is offered, with its Chinese name', async () => {
  const works = [{id: 'w0', tag: '', name: '未核对', count: 9, watchable: false, watched: false},
    {id: null, tag: 'blue_archive', name: 'Blue Archive', count: 5, watchable: true, watched: false}];
  const c = guide({results: {characters: 2, with_character: 7}, names: {network: {granted_at: '2026-10-12'}}, works, index: {watch: {works: []}}});
  await c.loadGuide(true);
  c.drawNudge();
  assert.match(c.nodes.nudge.innerHTML, /你的图里《蔚蓝档案》最多（5 张）/);
  const busy = guide({results: {characters: 2, with_character: 7}, names: {network: {granted_at: 'x'}}, works, sync: {status: 'recognising'}});
  await busy.loadGuide(true);
  busy.drawNudge();
  assert.equal(busy.nodes.nudge.innerHTML, '', 'not while the sync is still running');
});

test('the tour speaks of the running sync at the tasks stop', async () => {
  const c = guide({step: 'tasks', sync: {status: 'recognising'}, results: {total: 30}});
  await c.loadGuide();
  assert.match(c.tourStop('tasks').text, /line:recognising/);
  assert.match(c.tourStop('welcome').next, /带我看看/);
});

test('after a stopped or failed sync the problem bar says nothing is lost and what to do next (#97)', async () => {
  for (const [sync, expected] of [
    [{status: 'interrupted', message: '任务已取消'}, /同步没有做完（任务已取消）.*再同步一次会接着处理剩下的/s],
    [{status: 'error', message: '文件夹不存在或无法访问：/gone'}, /同步出错了.*\/gone.*外接硬盘/s],
    [{status: 'error', message: 'disk I/O error'}, /诊断信息/],
  ]) {
    const c = guide({sync});
    await c.loadGuide();
    c.drawGuideNotice();
    assert.equal(c.nodes.guideNotice.hidden, false, sync.message);
    assert.match(c.nodes.guideNotice.innerHTML, expected);
    assert.match(c.nodes.guideNotice.innerHTML, /data-guide-sync/);
  }
  const fine = guide({sync: {status: 'done'}});
  await fine.loadGuide();
  fine.drawGuideNotice();
  assert.equal(fine.nodes.guideNotice.hidden, true);
});
