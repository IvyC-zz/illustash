// 数字和日期跟随页面语言；中文保留自己的措辞（core/format.js；#182、#199）。
// 英文页面上的同样检查在 tests/english_page.test.cjs。
const {test, before} = require('node:test');
const assert = require('node:assert/strict');
const web = require('./web.cjs');
before(web.ready);

test('a Chinese page writes dates its own way', () => {
  const {number, dateText, monthText, esc, work} = web.core();
  assert.equal(number(12345), '12,345');
  assert.equal(dateText(2025, 5, 3, true) + '|' + dateText(2026, 5, 3) + '|' + dateText('2026', '05', '03') + '|' + monthText('2026', '05'),
    '2025 年 5 月 3 日|5 月 3 日|05 月 03 日|2026 年 05 月');
  assert.equal(esc(`<a href="x">'&'</a>`), '&lt;a href=&quot;x&quot;&gt;&#39;&amp;&#39;&lt;/a&gt;');
  assert.equal(esc(null), '');
  assert.equal(work('fate_(series)'), 'fate (series)');
});
