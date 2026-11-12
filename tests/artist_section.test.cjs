// 详情面板里带「也许是 TA」建议的画师区（ui/artists.js）：它的链接曾读取未定义的 TIPT（#173 改用 t() 时遗留），
// 所以凡是带 d.maybe 的图片都会抛出 ReferenceError，这一区始终画不出来。
// 英文页面见 tests/english_page.test.cjs。
const {test, before} = require('node:test');
const assert = require('node:assert/strict');
const web = require('./web.cjs');
let artists;
before(async () => { await web.ready(); artists = await web.load('ui/artists.js'); });
const maybe = {id: 'a1', name: '合成画师', reason: '合成理由', homes: [{url: 'https://example.com/a1', label: 'example', site: 'Example'}]};

test('a maybe artist renders its line and library link in Chinese', () => {
  const html = artists.artistSection({artists: [], posts: [], maybe: [maybe]}, {});
  assert.match(html, /也许是 <a class="artist-name" href="#\/library\?artist=a1" title="在图库里只看 TA 的图">合成画师<\/a>/);
  assert.match(html, /还没确定画师/);
});

test('a confirmed artist next to a maybe one keeps the same link title', () => {
  const html = artists.artistSection({artists: [{...maybe, id: 'a2', name: '合成画师二', count: 1, confirmed: true}], posts: [], maybe: [maybe]}, {});
  assert.equal(html.split('title="在图库里只看 TA 的图"').length - 1, 2);
});
