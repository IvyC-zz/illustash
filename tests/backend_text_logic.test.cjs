// What the backend decides must not hang on the words it shows (#204), the Rust counterpart of the page's rule in
// tests/i18n.test.cjs: rewording or translating a message must not change what the program does. Outside test
// modules, no Chinese string literal is compared (==, !=, a match pattern) or searched for (contains, starts_with, …);
// branch on an error's kind (crates/core/src/error.rs Kind) or a stored field instead.
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
// [line, text, the code just before, the code just after] for each Chinese string literal (escapes left as written);
// comments and char literals are skipped, and so is the file's test module.
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
// A literal handed to a search, or one side of a comparison or a match pattern. A literal in a list is judged by
// what the list is for: ["…", "…"].contains(&message) searches.
const SEARCH = /\.(?:contains|starts_with|ends_with|strip_prefix|strip_suffix|find|rfind|split_once|rsplit_once|matches|trim_start_matches|trim_end_matches)\(\s*&?$/;
const COMPARED = /(?:[=!]=)\s*&?$/;
const COMPARING = /^\s*(?:[=!]=|=>|\|\s*"|if\b)/;
const LISTED = /^(?:\s*,\s*"[^"\n]*")*\s*\]\s*\.(?:contains|iter\(\)\.any)\(/;
// Chinese that is someone else's data, not ours to word: what the program reads, not what it shows.
const OTHERS = new Map([
  // A field name of Bangumi's infobox, and words in a character's name that mark a group of characters.
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
