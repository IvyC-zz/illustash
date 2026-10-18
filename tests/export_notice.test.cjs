// #41: the library card says what a shareable export leaves out (images, provenance cache, imported X follows)
// and that moving to another computer needs the X archive imported again, once per state of the export.
const {test, before} = require('node:test');
const web = require('./web.cjs');   // the import's words and state are core/ (#199)
let settings;
before(async () => { await web.ready(); settings = await web.load('ui/settings.js'); });
const assert = require('node:assert/strict');

// The library card is ui/settings.js.
function render(exportState) {
  const card = {innerHTML: '', contains: () => false}, {state} = web.core();
  Object.assign(state, {route: {page: 'settings'}, importTask: undefined, libraries: {
    info: {name: '合成图库', path: '/tmp/synthetic.pilib', sources: [], test: null},
    current: '/tmp/synthetic.pilib', home: '/tmp', libraries: [], export: exportState}});
  globalThis.document = {getElementById: id => (id === 'setLibraries' ? card : null), activeElement: null};
  settings.renderLibrariesCard();
  return card.innerHTML;
}
const notice = '不含图片、画师出处缓存和导入过的 X 关注列表；在新电脑上要回查画师，需要重新导入 X 归档。';
const count = html => html.split(notice).length - 1;

test('the export notice appears once whatever the export state', () => {
  for (const exportState of [{status: 'idle'}, {status: 'running', path: '/tmp/a.zip'}, {status: 'error', error: '合成错误'}]) {
    assert.equal(count(render(exportState)), 1, exportState.status);
  }
});

test('a finished export repeats the notice next to its path instead of below', () => {
  const html = render({status: 'done', path: '/tmp/synthetic.pilib.zip', bytes: 3 << 20});
  assert.equal(count(html), 1);
  assert.match(html, /已导出：\/tmp\/synthetic\.pilib\.zip（3 MB）。不含图片/);
});
