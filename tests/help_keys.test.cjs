// 帮助对话框的快捷键表（#353，已知问题 11）：由 core/modes.js 的 SHORTCUTS 生成，和 ui/grid.js 的键盘处理认的键
// 一一对应；⌘ 在别的系统上写成 Ctrl。
const {test, before} = require('node:test');
const assert = require('node:assert/strict');
const {readFileSync} = require('node:fs');
const path = require('node:path');
const web = require('./web.cjs');
let modes, page;
before(async () => {
  await web.ready();
  modes = await web.load('core/modes.js');
  page = web.fakePage();
  globalThis.window = {addEventListener() {}};
  globalThis.ResizeObserver = class { observe() {} };
  globalThis.requestAnimationFrame = () => 0;
  const store = new Map();
  globalThis.localStorage = {getItem: k => store.has(k) ? store.get(k) : null, setItem: (k, v) => store.set(k, v), removeItem: k => store.delete(k)};
  (await web.load('ui/grid.js')).wireGrid();
});

// ui/grid.js 里 document 的 keydown 处理认哪些 e.key：`key === 'x'`、`e.key === 'Escape'`、`[…].includes(key)`、`/^[1-9]$/`。
function handledKeys() {
  const src = readFileSync(path.join(__dirname, '../web/static/ui/grid.js'), 'utf8');
  const start = src.indexOf("document.addEventListener('keydown'");
  assert.ok(start > 0, '找不到 ui/grid.js 的键盘处理');
  const body = src.slice(start, src.indexOf('\n  });\n', start));
  const keys = new Set();
  for (const m of body.matchAll(/\b(?:e\.)?key\s*[!=]==\s*'([^']+)'/g)) keys.add(m[1]);
  for (const m of body.matchAll(/\[([^\]]*)\]\.includes\((?:e\.)?key\)/g)) for (const k of m[1].matchAll(/'([^']*)'/g)) keys.add(k[1]);
  if (body.includes('/^[1-9]$/.test(e.key)')) keys.add('1-9');
  return keys;
}

test('帮助里列的键和键盘处理认的键一样多', () => {
  const declared = new Set(modes.SHORTCUTS.flatMap(s => s.codes));
  const handled = handledKeys();
  assert.ok(handled.size > 20, `只认出 ${handled.size} 个键，检查 handledKeys() 的写法是否还对得上 ui/grid.js`);
  for (const key of handled) assert.ok(declared.has(key), `键盘处理认「${key}」，但 core/modes.js 的 SHORTCUTS 里没有：帮助对话框会漏掉它`);
  for (const key of declared) assert.ok(handled.has(key), `SHORTCUTS 里有「${key}」，但 ui/grid.js 的键盘处理不认它：帮助对话框写了不存在的快捷键`);
  // 单图／选择快捷键（keyAction）的每个键都在表里
  for (const key of ['c', 'a', 't', 'g', 'x', 's', 'Backspace', 'Delete']) assert.ok(declared.has(key), key);
});

test('帮助对话框的内容就是 SHORTCUTS，⌘ 按本机系统显示', () => {
  const {isMac, platformText} = web.core(), html = page.el('helpKeys').innerHTML;
  assert.equal((html.match(/<dt>/g) || []).length, modes.SHORTCUTS.length);
  for (const s of modes.SHORTCUTS) assert.ok(html.includes(platformText(s.does())), s.does());
  assert.ok(html.includes(`<dt>${isMac ? '⌘K' : 'Ctrl+K'}</dt>`));
  // 别的系统上：⌘K 写成 Ctrl+K，单独的 ⌘ 写成 Ctrl
  assert.equal(platformText('⌘+ / ⌘−', false), 'Ctrl++ / Ctrl+−');
  assert.equal(platformText('捏合 · ⌘ 滚动', false), '捏合 · Ctrl 滚动');
  assert.equal(platformText('⌘K', true), '⌘K');
  // index.html 里不再手写快捷键表
  const index = readFileSync(path.join(__dirname, '../web/static/index.html'), 'utf8');
  assert.match(index, /<dl class="shortcuts" id="helpKeys"><\/dl>/);
});
