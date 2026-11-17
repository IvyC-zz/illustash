// #427：相似组的「看过了」。列表不显示看过的组，「看过的 N 组」只列它们；组页标题上标记、取消，撤销后列表重画。
// 页面是假的（tests/web.cjs），组和图都是合成的。
const {test, before} = require('node:test');
const assert = require('node:assert/strict');
const web = require('./web.cjs');
before(web.ready);

const tick = () => new Promise(resolve => setImmediate(resolve));
let shell, pages, page, state, requests, posts, groups;
async function wired() {
  if (shell) return;
  page = web.fakePage();
  globalThis.window = {addEventListener() {}};
  globalThis.history = {replaceState() {}};
  globalThis.requestAnimationFrame = () => 0;
  globalThis.matchMedia = () => ({matches: true});
  globalThis.IntersectionObserver = class { observe() {} unobserve() {} disconnect() {} };
  const store = new Map();
  globalThis.localStorage = {getItem: k => store.has(k) ? store.get(k) : null, setItem: (k, v) => store.set(k, v), removeItem: k => store.delete(k)};
  ({state} = web.core());
  shell = await web.load('ui/shell.js'); await web.load('ui/review.js'); pages = await web.load('core/pages.js');
  globalThis.fetch = (path, init) => {
    const q = new URLSearchParams(path.split('?')[1] || '');
    let body = [];
    if (init?.method === 'POST') {
      posts.push([path, JSON.parse(init.body)]);
      body = {undo: {id: posts.length, label: '已标为看过', ts: ''}};
    } else {
      requests.push(path);
      if (path.startsWith('/api/similar?')) body = {groups: groups.map(g => q.get('seen') === '1' ? g : (({seen, ...rest}) => rest)(g)), state: {status: 'done'}};
      else if (path.startsWith('/api/assets?')) body = {total: 2, items: [{uuid: 'a', memberships: [], width: 3, height: 4}, {uuid: 'b', memberships: [], width: 3, height: 4}]};
      else if (path.startsWith('/api/assets/count')) body = {total: 2};
    }
    return Promise.resolve({status: 200, ok: true, json: async () => body});
  };
}
async function visit(hash) {
  requests = []; posts = [];
  location.hash = hash;
  await shell.applyRoute(); await tick(); await tick();
}
const group = (gid, seen) => ({gid, size: 2, covers: ['a', 'b'], kinds: {duplicate: 1}, seen});

test('看过的组不在列表里，「看过的 N 组」只列它们', async () => {
  await wired();
  globalThis.location = {hash: ''};
  Object.assign(state, {route: {page: 'home'}, nsfw: '', guide: undefined});
  groups = [group(3, false), group(7, true)];
  await visit('#/similar');
  assert.ok(requests.some(p => p.startsWith('/api/similar?') && p.includes('seen=1')));
  const list = page.el('groupsPage').innerHTML;
  assert.match(list, /href="#\/s\/3"/);
  assert.doesNotMatch(list, /href="#\/s\/7"/);
  assert.match(list, /href="#\/similar\?seen=1">看过的 1 组 →/);
  await visit('#/similar?seen=1');
  const seen = page.el('groupsPage').innerHTML;
  assert.match(seen, /href="#\/s\/7"/);
  assert.doesNotMatch(seen, /href="#\/s\/3"/);
  assert.match(seen, /看过的 1 组 · 共 2 张/);
  assert.match(seen, /没看过的 1 组 →/);
  groups = [group(7, true)];
  await visit('#/similar');
  assert.match(page.el('groupsPage').innerHTML, /相似的组都看过了。/);
});

test('撤销之后相似的图页重画（#386 的 2-41）', async () => {
  await wired();
  groups = [group(3, false)];
  await visit('#/similar');
  groups = [group(3, false), group(4, false)];
  requests = [];
  await pages.pageOf(state.route).refresh('undo');
  assert.ok(requests.some(p => p.startsWith('/api/similar?')));
  assert.match(page.el('groupsPage').innerHTML, /href="#\/s\/4"/);
});

test('组页标题的「看过了」标记这组，回到列表；看过的组按钮是「标为没看过」', async () => {
  await wired();
  groups = [group(7, false)];
  await visit('#/s/7');
  assert.deepEqual(state.sgroupSeen, {seen: false, size: 2, nsfw: ''});
  assert.match(page.el('heading').innerHTML, /id="sgroupSeen"[^>]*>看过了</);
  await page.el('sgroupSeen').listeners.click.at(-1)();
  assert.deepEqual(posts, [['/api/similar/seen', {gid: 7, size: 2, nsfw: '', seen: true}]]);
  assert.equal(location.hash, '#/similar');
  groups = [group(7, true)];
  await visit('#/s/7');
  assert.match(page.el('heading').innerHTML, /id="sgroupSeen"[^>]*>标为没看过</);
  await page.el('sgroupSeen').listeners.click.at(-1)();
  assert.deepEqual(posts, [['/api/similar/seen', {gid: 7, size: 2, nsfw: '', seen: false}]]);
  assert.equal(location.hash, '#/s/7', '取消以后留在这一组');
  assert.match(page.el('heading').innerHTML, /id="sgroupSeen"[^>]*>看过了</);
  // 组不在了（组号重建）：不显示按钮。
  groups = [];
  await visit('#/s/7');
  assert.equal(state.sgroupSeen, null);
  assert.doesNotMatch(page.el('heading').innerHTML, /sgroupSeen/);
});
