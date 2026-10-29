// 后端的判断不能取决于它显示的文字（#204），这是页面规则（tests/i18n.test.cjs）在 Rust 一侧的对应：
// 改写或翻译一条消息不能改变程序的行为。测试模块之外，不拿中文字符串字面量做比较（==、!=、match 模式），
// 也不拿它搜索（contains、starts_with 等）；要分支就给失败一个 kind，或者读取存储的字段（docs/WEB_V2.md）。
const {test} = require('node:test');
const assert = require('node:assert/strict');
const {readFileSync, readdirSync} = require('node:fs');
const path = require('node:path');

const ROOT = path.join(__dirname, '..');
function rustFiles() {
  const out = [];
  const walk = dir => {
    for (const entry of readdirSync(dir, {withFileTypes: true})) {
      const full = path.join(dir, entry.name);
      if (entry.isDirectory()) { if (entry.name !== 'tests') walk(full); }
      else if (entry.name.endsWith('.rs') && entry.name !== 'tests.rs') out.push(full);
    }
  };
  walk(path.join(ROOT, 'native/terms/src'));
  walk(path.join(ROOT, 'desktop/src'));
  for (const crate of readdirSync(path.join(ROOT, 'crates'))) walk(path.join(ROOT, 'crates', crate, 'src'));
  return out.sort();
}
// 每个中文字符串字面量的 [行号, 文本, 紧挨着的前文代码, 紧挨着的后文代码]（转义按原样保留）；
// 跳过注释、字符字面量和文件里的测试模块。
function chineseLiterals(file) {
  let src = readFileSync(file, 'utf8');
  const tests = src.search(/\n#\[cfg\(test\)\]\s*\n\s*(?:pub(?:\(crate\))? )?mod \w+\s*\{/);
  if (tests >= 0) src = src.slice(0, tests);
  const out = [];
  let line = 1;
  for (let i = 0; i < src.length;) {
    const c = src[i], rest = src.slice(i, i + 12);
    if (c === '\n') { line++; i++; continue; }
    if (rest.startsWith('//')) { const j = src.indexOf('\n', i); i = j < 0 ? src.length : j; continue; }
    if (rest.startsWith('/*')) { const j = src.indexOf('*/', i); line += src.slice(i, j).split('\n').length - 1; i = j + 2; continue; }
    const raw = !/[\w]/.test(src[i - 1] || '') && rest.match(/^b?r(#*)"/);
    let start, end, close;
    if (raw) [start, end] = [i + raw[0].length, src.indexOf('"' + raw[1], i + raw[0].length)], close = end + 1 + raw[1].length;
    else if (c === "'") { const char = rest.match(/^'(?:\\u\{[0-9a-fA-F]+\}|\\.|[^\\'])'/); i += char ? char[0].length : 1; continue; }
    else if (c === '"') {
      let j = i + 1;
      while (src[j] !== '"') j += src[j] === '\\' ? 2 : 1;
      [start, end, close] = [i + 1, j, j + 1];
    } else { i++; continue; }
    const text = src.slice(start, end);
    if (/\p{Script=Han}/u.test(text)) out.push([line, text, src.slice(Math.max(0, i - 40), i), src.slice(close, close + 40)]);
    line += text.split('\n').length - 1;
    i = close;
  }
  return out;
}
// 交给搜索的字面量，或者比较、match 模式的一边。列表里的字面量按列表的用途判断：
// ["…", "…"].contains(&message) 算搜索。
const SEARCH = /\.(?:contains|starts_with|ends_with|strip_prefix|strip_suffix|find|rfind|split_once|rsplit_once|matches|trim_start_matches|trim_end_matches)\(\s*&?$/;
const COMPARED = /(?:[=!]=)\s*&?$/;
const COMPARING = /^\s*(?:[=!]=|=>|\|\s*"|if\b)/;
const LISTED = /^(?:\s*,\s*"[^"\n]*")*\s*\]\s*\.(?:contains|iter\(\)\.any)\(/;
// 别人的数据里的中文，不由我们措辞：是程序读取的内容，不是它显示的内容。
const OTHERS = new Map([
  // Bangumi infobox 的一个字段名，以及角色名里表示一组角色的词。
  ['native/terms/src/app_name_sources.rs', new Set(['简体中文名', '兄妹', '姐妹', '兄弟'])],
]);

test('no backend logic tests the Chinese text it shows', () => {
  const found = [];
  for (const file of rustFiles()) {
    const name = path.relative(ROOT, file).split(path.sep).join('/');
    for (const [line, text, before, after] of chineseLiterals(file)) {
      if (OTHERS.get(name)?.has(text)) continue;
      if (SEARCH.test(before) || COMPARED.test(before) || COMPARING.test(after) || (/[[,]\s*$/.test(before) && LISTED.test(after)))
        found.push(`${name}:${line}: "${text}"`);
    }
  }
  assert.deepEqual(found, []);
});
