// The core layer on an English page (#173, #182, #199), and the 画师 section's 「也许是 TA」 line (ui/artists.js, #223);
// tests/format.test.cjs, tests/session_switch.test.cjs and tests/artist_section.test.cjs are the Chinese page.
const {test, before} = require('node:test');
const assert = require('node:assert/strict');
const web = require('./web.cjs');
web.language('en');
before(web.ready);

test('numbers and dates follow Intl', () => {
  const {number, dateText, monthText, I18N} = web.core();
  assert.equal(I18N.language, 'en');
  assert.equal(number(12345) + '|' + dateText(2025, 5, 3, true) + '|' + dateText('2026', '05', '03') + '|' + monthText('2026', '05'),
    '12,345|5/3/2025|5/3|May 2026');
});

test('requests name the language; a refusal is shown translated and kept as sent for said()', async () => {
  const {api, said} = web.core(), sent = [];
  globalThis.fetch = async (path, options) => { sent.push(options.headers); return {status: 400, ok: false, json: async () => ({error: '冲突：会话已过期，请刷新页面'})}; };
  const refused = await api('/api/tag').catch(e => e);
  assert.equal(sent[0]['X-Illustash-Language'], 'en');
  assert.equal(refused.message, 'Conflict: The session has expired; reload the page');
  assert.doesNotMatch(refused.message, /\p{Script=Han}/u);
  assert.equal(refused.said, '冲突：会话已过期，请刷新页面');
  assert.ok(said(refused.said, '会话已过期，请刷新页面'));
});

test('a maybe artist renders in English too', async () => {
  const {artistSection} = await web.load('ui/artists.js');
  const maybe = {id: 'a1', name: '合成画师', reason: '合成理由', homes: [{url: 'https://example.com/a1', label: 'example', site: 'Example'}]};
  const html = artistSection({artists: [], posts: [], maybe: [maybe]}, {});
  assert.match(html, /Maybe <a class="artist-name" href="#\/library\?artist=a1" title="Show only their pictures in the library">合成画师<\/a>/);
});
