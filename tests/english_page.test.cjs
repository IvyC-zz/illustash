// 英文页面上的核心层（#173、#182、#199），以及画师区的「也许是 TA」一行（ui/artists.js，#223）；
// 中文页面见 tests/format.test.cjs、tests/session_switch.test.cjs 和 tests/artist_section.test.cjs。
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

test('the time recognition still needs is told in minutes or, when long, in hours (#360)', () => {
  const {syncLine} = web.core();
  const line = (total, ms) => syncLine({status: 'recognising', stage: 'recognising', total, done: 0, ms_per_image: ms});
  assert.equal(line(30, 60000), 'Recognizing new pictures 0/30 · about 30 min');
  assert.equal(line(50000, 940), 'Recognizing new pictures 0/50,000 · about 13 h');
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

// #519：GPU 回退原因是后端的中文原话，按后端文字翻译；一张图时用单数；关于页没检查过更新时是完整的一句。
test('a GPU fallback reason, one picture and a never-checked update read as English (#519)', async () => {
  const {t, state} = web.core();
  const {accelerationLine} = await web.load('ui/models.js');
  const reason = 'WD 加速文件缺失或损坏；在设置的识别模型中重新下载或导入加速文件';
  const line = accelerationLine({acceleration: {available: true, backend: 'coreml', on: true, last: {wd: {requested: 'coreml', used: 'cpu', reason, at: 1}}}});
  assert.match(line, /<em>Last time the GPU could not be used, so the CPU was used instead: The WD acceleration file is missing or damaged; download or import it again under Recognition models in Settings<\/em>/);
  assert.doesNotMatch(line, /\p{Script=Han}/u);
  assert.equal(t('{n} 张 · 只是标记，不改动原图', {n: '1'}), '1 picture · only marked, the original file is not touched');
  assert.equal(t('{n} 张 · 只是标记，不改动原图', {n: '2'}), '2 pictures · only marked, the original files are not touched');
  assert.equal(t('共 {n} 份备份', {n: '1'}), '1 backup');
  const {updatesLine} = await web.load('ui/settings.js');
  state.updates = {checked_at: null, newer: false, auto: false};
  assert.match(updatesLine(), /Not checked yet · Automatic checks are off/);
  assert.equal(t('还没有检查过'), 'never', 'the integrity line still says “library.sqlite never”');
});
