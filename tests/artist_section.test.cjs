// The inspector's 画师 section with a 「也许是 TA」 suggestion: its link once read an undefined TIPT (left by the
// #173 move to t()), so any picture with d.maybe threw a ReferenceError and the section was never drawn.
const {test} = require('node:test');
const assert = require('node:assert/strict');
const {readFileSync} = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const read = name => readFileSync(path.join(__dirname, '../web/static', name), 'utf8');
const source = read('app.js');
const section = (a, b) => source.slice(source.indexOf(a), source.indexOf(b, source.indexOf(a)));

function render(language, d) {
  const english = JSON.parse(read('locales/en.json'));
  const c = {self: {ILLUSTASH_LANGUAGE: language, ILLUSTASH_LANGUAGES: {en: english['@name']}, ILLUSTASH_TABLES: {en: english}},
    navigator: {languages: [language]}, document: {documentElement: {}, querySelectorAll: () => []},
    esc: s => String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/"/g, '&quot;'),
    hashFor: route => '#/' + route.page + '?artist=' + route.artist, number: String};
  vm.createContext(c);
  vm.runInContext(read('i18n.js'), c);
  vm.runInContext(section('function artistSection(d, ident)', '// ---------- 检查更新'), c);
  return c.artistSection(d, {});
}
const maybe = {id: 'a1', name: '合成画师', reason: '合成理由', homes: [{url: 'https://example.com/a1', label: 'example', site: 'Example'}]};

test('a maybe artist renders its line and library link in Chinese', () => {
  const html = render('zh-CN', {artists: [], posts: [], maybe: [maybe]});
  assert.match(html, /也许是 <a class="artist-name" href="#\/library\?artist=a1" title="在图库里只看 TA 的图">合成画师<\/a>/);
  assert.match(html, /还没确定画师/);
});

test('a maybe artist renders in English too', () => {
  const html = render('en', {artists: [], posts: [], maybe: [maybe]});
  assert.match(html, /Maybe <a class="artist-name" href="#\/library\?artist=a1" title="Show only their pictures in the library">合成画师<\/a>/);
});

test('a confirmed artist next to a maybe one keeps the same link title', () => {
  const html = render('zh-CN', {artists: [{...maybe, id: 'a2', name: '合成画师二', count: 1, confirmed: true}], posts: [], maybe: [maybe]});
  assert.equal(html.split('title="在图库里只看 TA 的图"').length - 1, 2);
});
