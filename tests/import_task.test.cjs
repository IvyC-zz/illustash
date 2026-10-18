// #88: an archive or recognition-result import runs as a background task. The library card shows the step it is on
// with real counts only, a cancel button while it runs and how it ended; late or out-of-order answers (#34) never wind
// it back, and the end is announced once.
const {test, before, afterEach} = require('node:test');
const web = require('./web.cjs');   // the import's poller and words are core/jobs.js (#199)
let settings, effects;
before(async () => { await web.ready(); settings = await web.load('ui/settings.js'); effects = await web.load('ui/effects.js'); });
let cleanup = () => {};
afterEach(() => { cleanup(); cleanup = () => {}; });
const assert = require('node:assert/strict');
const esc = s => String(s).replace(/[&<>"]/g, c => ({'&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;'})[c]);

// The library card is ui/settings.js; the import's words and state are core/jobs.js.
function card(importTask) {
  const node = {innerHTML: '', contains: () => false}, {state, importLine} = web.core();
  Object.assign(state, {route: {page: 'settings'}, importTask, libraries: {
    info: {name: '合成图库', path: '/tmp/synthetic.pilib', sources: [], test: null},
    current: '/tmp/synthetic.pilib', home: '/tmp', libraries: [], export: {status: 'idle'}}});
  globalThis.document = {getElementById: id => (id === 'setLibraries' ? node : null), activeElement: null};
  settings.renderLibrariesCard();
  return {html: node.innerHTML, line: importLine};
}
const running = (extra = {}) => ({status: 'running', kind: 'archive', id: 't1', file: '合成.zip', ...extra});

test('a running import shows its stage with real counts and a cancel button, and blocks another import', () => {
  const {html, line} = card(running({stage: 'unpacking', done: 3 << 20, total: 256 << 20}));
  assert.match(html, /导入归档「合成\.zip」：正在解压 3\.0\/256 MB…/);
  assert.match(html, /data-import-cancel/);
  assert.match(html, /data-lib-choose="zip" disabled/);
  assert.equal(line(running()), '导入归档「合成.zip」：正在准备…', 'no stage yet: no invented progress');
  assert.equal(line(running({stage: 'checking'})), '导入归档「合成.zip」：正在检查新图库能否打开…');
  const recognition = {status: 'running', kind: 'recognition', id: 't2', file: 'result'};
  assert.equal(line({...recognition, stage: 'models'}), '导入识别结果「result」：正在核对模型指纹…');
  assert.equal(line({...recognition, stage: 'reading', done: 1, total: 3}), '导入识别结果「result」：正在读取结果包（第 2/3 个文件）…');
  assert.equal(line({...recognition, stage: 'writing', done: 40, total: 120}), '导入识别结果「result」：正在写入识别结果 40/120 张…');
});

test('a stopping or finished import has no cancel button and says how it ended', () => {
  for (const [task, text] of [
    [{...running(), status: 'stopping', stage: 'unpacking'}, '正在取消导入归档「合成.zip」…'],
    [{...running(), status: 'cancelled', reason: '图库已切换，导入已取消'}, '图库已切换，导入已取消；图库没有改动。'],
    [{...running(), status: 'error', error: '导入的图库无法打开，已取消导入', code: 400}, '导入归档「合成.zip」失败：导入的图库无法打开，已取消导入'],
    [{...running(), status: 'done', result: {name: '合成', path: '/tmp/合成.pilib'}}, '已导入「合成」，模型接口与名称联网授权需重新设置。'],
    [{status: 'done', kind: 'recognition', id: 't2', result: {status: 'done', duplicate: true}}, '这份识别结果之前已经导入过。'],
    [{status: 'done', kind: 'recognition', id: 't2', result: {status: 'pending', error: '合成'}}, '识别结果已导入，后续处理尚未完成（合成）。'],
  ]) {
    const {html} = card(task);
    assert.ok(html.includes(esc(text)), `${task.status}: ${html}`);
    assert.doesNotMatch(html, /data-import-cancel/);
    // A stopping import still holds the slot, so a new one cannot start until it has ended.
    (task.status === 'stopping' ? assert.match : assert.doesNotMatch)(html, /data-lib-choose="zip" disabled/);
  }
  assert.doesNotMatch(card({status: 'idle'}).html, /import-line/);
  assert.doesNotMatch(card({...running(), status: 'done', result: {name: '合成', path: '/tmp/合成.pilib'}}).html, /data-open-library/);
  assert.doesNotMatch(card(undefined).html, /import-line/);
});

// core/jobs.js polls; the library card (ui/settings.js) starts an import; ui/effects.js afterJob(), as app.js wires it,
// announces the end and lists a new library. The page is a fake one; the import's own timers are the ones counted.
function poller(token = 'session') {
  const answers = [], timers = [], toasts = [], posted = [], offs = [];
  let libraries = 0;
  const core = web.core(), {state, session, onJob} = core;
  session.token = token;
  Object.assign(state, {route: {page: 'settings'}, importTask: undefined});
  const page = web.fakePage();
  Object.defineProperty(page.el('toastText'), 'textContent', {set: m => toasts.push(m), get: () => toasts.at(-1) || ''});
  globalThis.fetch = (path, options) => {
    if (path === '/api/imports/cancel') { posted.push(options); return Promise.resolve({status: 200, ok: true, json: async () => ({status: 'stopping'})}); }
    if (path === '/api/libraries/import') return Promise.resolve({status: 200, ok: true, json: async () => ({started: true, task: running()})});
    if (path === '/api/libraries') { libraries++; return Promise.resolve({status: 200, ok: true, json: async () => ({})}); }
    return new Promise(resolve => answers.push(body => resolve({status: 200, ok: true, json: async () => body})));
  };
  const setTimeout = globalThis.setTimeout;
  globalThis.setTimeout = (fn, ms) => { if (ms === 700 || ms === 5000) timers.push(ms); return timers.length; };
  offs.push(onJob(effects.afterJob));
  const c = {state, session, pollImports: core.pollImports, cancelImport: core.cancelImport, startImport: settings.startImport};
  cleanup = () => { globalThis.setTimeout = setTimeout; for (const off of offs) off(); };
  return {c, answers, timers, toasts, posted, libraries: () => libraries};
}
test('starting polls until the end; an older answer arriving last does not wind the state back', async () => {
  const p = poller();
  await p.c.startImport('/tmp/合成.zip');
  assert.equal(p.c.state.importTask.status, 'running');
  const older = p.c.pollImports();  // a second poll overlaps the first one
  p.answers[1]({...running(), status: 'done', result: {name: '合成'}});
  await older;
  p.answers[0](running({stage: 'unpacking', done: 1, total: 2}));
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(p.c.state.importTask.status, 'done');
  assert.deepEqual(p.toasts, ['已导入「合成」，模型接口与名称联网授权需重新设置。']);
  assert.equal(p.libraries(), 1, 'the new library is listed once it is registered');
  assert.deepEqual(p.timers, [], 'finished: nothing polls again, and the stale answer schedules nothing');
});

test('a running answer schedules the next poll and the end is announced only once', async () => {
  const p = poller();
  p.c.state.importTask = running();
  const first = p.c.pollImports();
  p.answers[0](running({stage: 'unpacking', done: 1, total: 2}));
  await first;
  assert.deepEqual(p.timers, [700]);
  const second = p.c.pollImports();
  p.answers[1]({...running(), status: 'cancelled', reason: '导入已取消'});
  await second;
  const third = p.c.pollImports();
  p.answers[2]({...running(), status: 'cancelled', reason: '导入已取消'});
  await third;
  assert.deepEqual(p.toasts, ['导入已取消；图库没有改动。']);
});

test('an answer for a page whose session changed meanwhile is dropped', async () => {
  const p = poller();
  p.c.state.importTask = running();
  const pending = p.c.pollImports();
  p.c.session.token = 'replacement';
  p.answers[0]({...running(), status: 'done', result: {name: '合成'}});
  await pending;
  assert.equal(p.c.state.importTask.status, 'running');
  assert.deepEqual(p.toasts, []);
});

test('the cancel button asks the server and then polls for the end', async () => {
  const p = poller();
  p.c.state.importTask = running();
  const cancelling = p.c.cancelImport();
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(p.posted.length, 1);
  assert.equal(p.posted[0].method, 'POST');
  p.answers[0]({...running(), status: 'cancelled', reason: '导入已取消'});
  await cancelling;
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(p.c.state.importTask.status, 'cancelled');
});

test('a published import that could not be listed is offered to open instead of importing again (#111)', () => {
  const task = {...running(), status: 'done', result: {name: '合成', path: '/Users/me/Pictures/Illustash/合成 2.pilib', listed: false, error: '写入失败（合成）'}};
  const {html, line} = card(task);
  assert.equal(line(task), '已导入「合成」，保存在 ~/Pictures/Illustash/合成 2.pilib，但没能加入最近图库列表：写入失败（合成）。打开它会再试一次加入列表；再导入会另建一个带编号的图库。模型接口与名称联网授权需重新设置。');
  assert.match(html, /data-open-library="\/Users\/me\/Pictures\/Illustash\/合成 2\.pilib" data-name="合成">打开这个图库/);
  assert.doesNotMatch(html, /data-lib-choose="zip" disabled/);
});
