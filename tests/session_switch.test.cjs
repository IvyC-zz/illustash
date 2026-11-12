// 页面始终绑定启动时的会话（#33）：图库一旦在别处切换或重新打开，
// 本页持有的任何内容都不能带着新会话的 token 发出。
const test = require('node:test');
const assert = require('node:assert/strict');
const web = require('./web.cjs');   // core/session.js 和 ui/dom.js（#199）
const tick = () => new Promise(resolve => setImmediate(resolve));
let dom;
test.before(async () => { await web.ready(); dom = await web.load('ui/dom.js'); web.fakePage(); dom.wireDom(); });

// 一个会话可被「另一个窗口」替换的服务端；每个请求都记录下来。请求和会话在 core/session.js，
// 横幅在 ui/dom.js，元数据刷新在 core/meta.js。
function harness() {
  const server = {token: 'session-a', chars: [{id: 'a-char'}], requests: [], hold: null};
  const nodes = {};
  const element = () => ({hidden: true, textContent: '', children: [], append(child) { this.children.push(child); }});
  const respond = (status, body) => ({status, ok: status < 400, json: async () => body});
  globalThis.document = {getElementById: id => nodes[id] ||= element(), createElement: element};
  globalThis.location = {reload() { c.reloaded = true; }};
  globalThis.fetch = async (path, options = {}) => {
    server.requests.push({path, method: options.method || 'GET', token: options.headers['X-CSRF-Token']});
    if (server.hold) await server.hold;
    if (options.method === 'POST' && options.headers['X-CSRF-Token'] !== server.token)
      return respond(403, {error: '会话已过期，请刷新页面'});
    if (path.startsWith('/api/bootstrap')) return respond(200, {token: server.token, undo: null, total: 1});
    if (path.startsWith('/api/characters')) return respond(200, server.chars);
    if (path.startsWith('/api/identity/count')) return respond(200, {total: 0});
    return respond(200, path === '/api/tag' ? {ok: true} : []);
  };
  // 元数据读取在 core/meta.js。
  const {session, adoptToken, staleSession, api, post, state, refreshMeta} = web.core();
  Object.assign(state, {nsfw: '', route: {page: 'library'}, char: null, chars: [], dupInfo: {}, exclInfo: {},
    readEpoch: 0, sel: {all: false, ids: new Set(['same-uuid']), exclude: new Set()}});
  const c = {session, adoptToken, staleSession, api, post, error: dom.error, state, refreshMeta};
  return {c, server, banner: () => nodes.errorBanner};
}
// 另一个窗口打开图库 B，它与 A 有相同的图片 UUID。
const switchElsewhere = server => { server.token = 'session-b'; server.chars = [{id: 'b-char'}]; };
const writes = server => server.requests.filter(r => r.method === 'POST');

test('a new session seen by a metadata refresh stops the page instead of adopting its token', async () => {
  const {c, server, banner} = harness();
  c.adoptToken((await c.api('/api/bootstrap')).token);
  await c.refreshMeta();
  assert.equal(c.state.chars[0].id, 'a-char');
  switchElsewhere(server);
  await c.refreshMeta();
  assert.equal(c.session.token, 'session-a');
  assert.equal(c.session.stale, true);
  assert.equal(c.state.chars[0].id, 'a-char');   // B 的行不会混进 A 的页面
  assert.match(banner().textContent, /其他窗口切换或重新打开/);
  assert.equal(banner().hidden, false);
  banner().children[0].onclick(); assert.equal(c.reloaded, true);
  // 保留的选择里有一个 B 也有的 UUID：写入在任何请求发出之前就被拒绝。
  await assert.rejects(c.post('/api/tag', {ids: [...c.state.sel.ids], tag: 'kept-selection'}), /重新载入页面/);
  assert.deepEqual(writes(server), []);
  c.error('later failure'); assert.match(banner().textContent, /其他窗口切换/);
});

test('a write rejected as an expired session also stops the page', async () => {
  const {c, server} = harness();
  c.adoptToken((await c.api('/api/bootstrap')).token);
  switchElsewhere(server);
  await assert.rejects(c.post('/api/tag', {ids: ['same-uuid'], tag: 'first'}), /重新载入页面/);
  assert.equal(c.session.stale, true);
  const sent = server.requests.length;
  await assert.rejects(c.api('/api/assets'), /重新载入页面/);
  await assert.rejects(c.post('/api/tag', {ids: ['same-uuid'], tag: 'second'}), /重新载入页面/);
  assert.equal(server.requests.length, sent);
  assert.deepEqual(writes(server).map(r => r.token), ['session-a']);
});

test('a response that arrives after the page stopped is dropped', async () => {
  const {c, server} = harness();
  c.adoptToken((await c.api('/api/bootstrap')).token);
  let release; server.hold = new Promise(resolve => release = resolve);
  const late = c.api('/api/assets');
  await tick();
  c.staleSession(); release();
  await assert.rejects(late, /重新载入页面/);
});

test('the same session keeps working and its token is adopted once', async () => {
  const {c, server} = harness();
  assert.equal(c.adoptToken('session-a'), true);
  await c.refreshMeta();
  assert.deepEqual(await c.post('/api/tag', {ids: ['same-uuid'], tag: 'ok'}), {ok: true});
  assert.equal(c.session.stale, false);
  assert.deepEqual(writes(server).map(r => r.token), ['session-a']);
});
