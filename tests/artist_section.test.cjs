// The inspector's 画师 section (ui/artists.js) with a 「也许是 TA」 suggestion: its link once read an undefined TIPT
// (left by the #173 move to t()), so any picture with d.maybe threw a ReferenceError and the section was never drawn.
// The English page is tests/english_page.test.cjs.
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
