// #41: the library card says what a shareable export leaves out (images, provenance cache, imported X follows)
// and that moving to another computer needs the X archive imported again, once per state of the export.
const {test} = require('node:test');
const assert = require('node:assert/strict');
const {readFileSync} = require('node:fs');
const vm = require('node:vm');
const zh = require('./i18n_zh.cjs');   // t() of a Chinese page (#173)
const source = readFileSync(require('node:path').join(__dirname, '../web/static/app.js'), 'utf8');
const section = (a, b) => source.slice(source.indexOf(a), source.indexOf(b, source.indexOf(a)));

function render(exportState) {
  const card = {innerHTML: ''};
  const c = {
    $: id => (id === 'setLibraries' ? card : null), esc: String, IS_MAC: false, devMode: false,
    origins: () => [['x', 'X']], cardHead: title => `<h2>${title}</h2>`,
    // #34 routes card markup through setSettingsHTML; render straight into the card here.
    setSettingsHTML: (card, html) => { card.innerHTML = html; return true; },
    libForm: {open: false, folders: {}, name: '', photos: false},
    state: {route: {page: 'settings'}, libraries: {
      info: {name: '合成图库', path: '/tmp/synthetic.pilib', sources: [], test: null},
      current: '/tmp/synthetic.pilib', home: '/tmp', libraries: [], export: exportState}},
  };
  vm.createContext(Object.assign(c, zh.page));
  vm.runInContext(section('function renderLibrariesCard()', '// Gallery pages have no Tauri dialog permission'), c);
  c.renderLibrariesCard();
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
