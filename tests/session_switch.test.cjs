// The page stays bound to the session it booted with (#33): once the library is switched or reopened
// elsewhere, nothing this page holds may be sent with the new session's token.
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const zh = require('./i18n_zh.cjs');   // t() of a Chinese page (#173)
const source = fs.readFileSync('web/static/app.js', 'utf8');
const section = (a, b) => source.slice(source.indexOf(a), source.indexOf(b, source.indexOf(a)));
const tick = () => new Promise(resolve => setImmediate(resolve));

// A server whose session can be replaced by "another window"; every request is recorded.
function harness() {
  const server = {token: 'session-a', chars: [{id: 'a-char'}], requests: [], hold: null};
  const nodes = {};
  const element = () => ({hidden: true, textContent: '', children: [], append(child) { this.children.push(child); }});
  const respond = (status, body) => ({status, ok: status < 400, json: async () => body});
  const c = {
    URLSearchParams, state: {nsfw: '', token: '', stale: false, route: {page: 'library'}, chars: [], dupInfo: {}, exclInfo: {},
      readEpoch: 0, sel: {all: false, ids: new Set(['same-uuid']), exclude: new Set()}},
    $: id => nodes[id] ||= element(), document: {createElement: element}, location: {reload() { c.reloaded = true; }}, guideAfterPost() {},
    setUndo() {}, navCount() {}, fetchChar: async () => null, renderChrome() {}, renderSettings() {}, renderIndexBody() {},
    async fetch(path, options = {}) {
      server.requests.push({path, method: options.method || 'GET', token: options.headers['X-CSRF-Token']});
      if (server.hold) await server.hold;
      if (options.method === 'POST' && options.headers['X-CSRF-Token'] !== server.token)
        return respond(403, {error: '会话已过期，请刷新页面'});
      if (path.startsWith('/api/bootstrap')) return respond(200, {token: server.token, undo: null, total: 1});
      if (path.startsWith('/api/characters')) return respond(200, server.chars);
      if (path.startsWith('/api/identity/count')) return respond(200, {total: 0});
      return respond(200, path === '/api/tag' ? {ok: true} : []);
    },
  };
  vm.createContext(Object.assign(c, {t: zh}));
  vm.runInContext(section('const SESSION_EXPIRED', 'function readError('), c);
  vm.runInContext(section('let metaPending', '// ---------- chrome'), c);
  return {c, server, banner: () => nodes.errorBanner};
}
// Another window opens library B, which shares A's image UUIDs.
const switchElsewhere = server => { server.token = 'session-b'; server.chars = [{id: 'b-char'}]; };
const writes = server => server.requests.filter(r => r.method === 'POST');

test('a new session seen by a metadata refresh stops the page instead of adopting its token', async () => {
  const {c, server, banner} = harness();
  c.adoptToken((await c.api('/api/bootstrap')).token);
  await c.refreshMeta();
  assert.equal(c.state.chars[0].id, 'a-char');
  switchElsewhere(server);
  await c.refreshMeta();
  assert.equal(c.state.token, 'session-a');
  assert.equal(c.state.stale, true);
  assert.equal(c.state.chars[0].id, 'a-char');   // B's rows never mix into A's page
  assert.match(banner().textContent, /其他窗口切换或重新打开/);
  assert.equal(banner().hidden, false);
  banner().children[0].onclick(); assert.equal(c.reloaded, true);
  // The kept selection names a UUID B also has: the write is refused before any request leaves.
  await assert.rejects(c.post('/api/tag', {ids: [...c.state.sel.ids], tag: 'kept-selection'}), /重新载入页面/);
  assert.deepEqual(writes(server), []);
  c.error('later failure'); assert.match(banner().textContent, /其他窗口切换/);
});

test('a write rejected as an expired session also stops the page', async () => {
  const {c, server} = harness();
  c.adoptToken((await c.api('/api/bootstrap')).token);
  switchElsewhere(server);
  await assert.rejects(c.post('/api/tag', {ids: ['same-uuid'], tag: 'first'}), /重新载入页面/);
  assert.equal(c.state.stale, true);
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
  assert.equal(c.state.stale, false);
  assert.deepEqual(writes(server).map(r => r.token), ['session-a']);
});
