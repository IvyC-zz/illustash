// Translation consistency (#173, #182): every t('…') literal and every data-t / data-t-attr text in the page matches
// each table in web/static/locales/<code>.json, which has no keys the code no longer uses, and whose placeholders,
// contexts and plural forms fit the Chinese. Text not yet translated falls back to Chinese: it is counted and reported
// per language, never a failure. The backend's own text (#173 stage 3) is a Chinese string literal in its Rust source
// (native/terms/src, crates/*/src); the page translates it with tb(), and its tables may hold those literals too.
const {test} = require('node:test');
const assert = require('node:assert/strict');
const {readFileSync, readdirSync} = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const STATIC = path.join(__dirname, '../web/static');
const read = name => readFileSync(path.join(STATIC, name), 'utf8');
// The desktop shell shares the tables (#173 stage 4): its start page calls t() like the library page, its Rust code
// calls tr("…") / tr_with("…", …) (desktop/src/i18n.rs).
const DESKTOP = path.join(__dirname, '../desktop');
const shellPage = readFileSync(path.join(DESKTOP, 'dist/index.html'), 'utf8');
const shellSources = readdirSync(path.join(DESKTOP, 'src')).filter(name => name.endsWith('.rs'))
  .map(name => ['desktop/src/' + name, readFileSync(path.join(DESKTOP, 'src', name), 'utf8')]);
// The same rule as native/terms/src/app_preferences.rs: a language code names the file.
const CODE = /^[a-z]{2,3}(-[A-Za-z0-9]{1,8})*$/;
const locales = Object.fromEntries(readdirSync(path.join(STATIC, 'locales')).filter(name => name.endsWith('.json'))
  .map(name => [name.slice(0, -5), JSON.parse(read('locales/' + name))]));
const english = locales.en;
// i18n.js itself only translates text handed to it.
const scripts = readdirSync(STATIC).filter(name => name.endsWith('.js') && name !== 'i18n.js');
const placeholders = text => [...text.matchAll(/\{(\w+)\}/g)].map(m => m[1]).sort();
const unescape = (quote, body) => vm.runInNewContext(quote + body + quote);

function scriptKeys(name, source = read(name)) {
  const keys = [], dynamic = [];
  for (const call of source.matchAll(/(?<![\w.$])t\(\s*(['"`]?)/g)) {
    const rest = source.slice(call.index + call[0].length - call[1].length);
    const literal = call[1] && rest.match(/^(['"])((?:\\.|(?!\1)[^\\\n])*)\1\s*[,)]/);
    // t('…', {context: 'x'}) is looked up as "x\u0004…" (gettext's msgctxt).
    const context = literal && rest.slice(literal[0].length - 1).match(/^,\s*\{\s*context:\s*'([\w-]+)'/);
    if (literal) keys.push((context ? context[1] + '\u0004' : '') + unescape(literal[1], literal[2]));
    else dynamic.push(`${name}:${source.slice(0, call.index).split('\n').length}`);
  }
  return {keys, dynamic};
}
function markupKeys(html) {
  const keys = [];
  for (const {0: tag, index} of html.matchAll(/<[a-z][^>]*>/g)) {
    if (/\sdata-t[\s>]/.test(tag)) keys.push(html.slice(index + tag.length).match(/^([^<]*)</)[1]);
    const attrs = tag.match(/\sdata-t-attr="([^"]*)"/);
    if (attrs) for (const name of attrs[1].split(' ')) {
      const value = tag.match(new RegExp(`\\s${name}="([^"]*)"`));
      assert.ok(value, `${tag} has no ${name}`);
      keys.push(value[1].replaceAll('&amp;', '&').replaceAll('&quot;', '"'));
    }
  }
  return keys;
}

// Every Chinese string literal of the backend's Rust source outside its test modules, as the program holds it.
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
  for (const crate of readdirSync(path.join(ROOT, 'crates'))) walk(path.join(ROOT, 'crates', crate, 'src'));
  return out.sort();
}
function rustUnescape(body) {
  return body.replace(/\\(?:\n\s*|u\{([0-9a-fA-F]+)\}|x([0-9a-fA-F]{2})|(.))/g, (all, code, hex, c) =>
    code ? String.fromCodePoint(parseInt(code, 16)) : hex ? String.fromCharCode(parseInt(hex, 16))
      : c === undefined ? '' : ({n: '\n', t: '\t', r: '\r', 0: '\0'})[c] ?? c);
}
// [file, line, text, the code just before it] for each literal; comments and char literals are skipped.
function rustLiterals(file) {
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
    if (raw) {
      const start = i + raw[0].length, end = src.indexOf('"' + raw[1], start), text = src.slice(start, end);
      out.push([file, line, text, src.slice(Math.max(0, i - 60), i)]);
      line += text.split('\n').length - 1; i = end + 1 + raw[1].length; continue;
    }
    if (c === "'") { const char = rest.match(/^'(?:\\u\{[0-9a-fA-F]+\}|\\.|[^\\'])'/); i += char ? char[0].length : 1; continue; }
    if (c === '"') {
      let j = i + 1;
      while (src[j] !== '"') j += src[j] === '\\' ? 2 : 1;
      const body = src.slice(i + 1, j);
      out.push([file, line, rustUnescape(body), src.slice(Math.max(0, i - 60), i)]);
      line += body.split('\n').length - 1; i = j + 1; continue;
    }
    i++;
  }
  return out.filter(([, , text]) => /\p{Script=Han}/u.test(text));
}
const rust = rustFiles().flatMap(rustLiterals);
// The release channel the about page shows comes from the build's metadata, not a literal.
const application = path.join(ROOT, 'native/terms/application.json');
rust.push([application, 1, JSON.parse(readFileSync(application, 'utf8')).channel, '']);
const backendKeys = new Set(rust.map(([, , text]) => text));
// Backend text that never reaches the page: the command line, test harnesses, logs and panics. Only the coverage
// report leaves it out; a table may still translate it.
const NOT_SHOWN = /^(?:native\/terms\/src\/(?:cli|read_harness)\.rs|native\/terms\/src\/bin\/|crates\/iqdb\/)/;
// SQL is left out too, and the pieces http_app.rs parses old undo labels with (" 张移出 ").
const shownBackend = () => new Set(rust.filter(([file, , text, before]) => !NOT_SHOWN.test(path.relative(ROOT, file).split(path.sep).join('/'))
  && !/(?:e?print(?:ln)?!|panic!|unreachable!|expect|assert\w*!)\(\s*$/.test(before)
  && !/\b(?:CREATE|SELECT|INSERT|UPDATE|DELETE)\b/.test(text) && text.trim() === text).map(([, , text]) => text));

// tr("…") in the shell's Rust: a plain string literal, its escapes read as JSON (\" and \\ are the same). Comment
// lines and the functions' own definitions are not calls.
function rustKeys(name, text) {
  const keys = [], dynamic = [];
  const source = text.split('\n').map(line => /^\s*\/\//.test(line) ? '' : line).join('\n');
  for (const call of source.matchAll(/(?<![\w])(fn\s+)?tr(?:_with)?\(/g)) {
    if (call[1]) continue;
    const rest = source.slice(call.index + call[0].length);
    const literal = rest.match(/^\s*"((?:\\.|[^"\\])*)"\s*[,)]/);
    if (literal) keys.push(JSON.parse(`"${literal[1]}"`));
    else dynamic.push(`${name}:${source.slice(0, call.index).split('\n').length}`);
  }
  return {keys, dynamic};
}
const shellScript = [...shellPage.matchAll(/<script>([\s\S]*?)<\/script>/g)].map(m => m[1]).join('\n');

function usedKeys() {
  const used = new Set(), dynamic = [];
  const add = found => { found.keys.forEach(k => used.add(k)); dynamic.push(...found.dynamic); };
  for (const name of scripts) add(scriptKeys(name));
  // i18n.js only translates what it is handed, apart from the joiner of backend messages.
  used.add('join\u0004；');
  markupKeys(read('index.html')).forEach(k => used.add(k));
  add(scriptKeys('desktop/dist/index.html', shellScript));
  markupKeys(shellPage).forEach(k => used.add(k));
  for (const [name, source] of shellSources) add(rustKeys(name, source));
  return {used, dynamic};
}

test('every translated text in the code is a plain literal', () => {
  const {used, dynamic} = usedKeys();
  // A key computed at run time cannot be checked or collected for translators.
  assert.deepEqual(dynamic, [], 't() must be called with a single- or double-quoted literal');
  assert.ok(used.size > 0);
  for (const key of used) {
    const context = key.split('\u0004');
    assert.ok(context.length === 1 || (context.length === 2 && /^[\w-]+$/.test(context[0]) && context[1]), `bad context: ${key}`);
    assert.ok(!key.startsWith('@'), `@ keys are the table's own: ${key}`);
  }
});

// A table entry is a string, or plural forms for {n}: the language's Intl.PluralRules categories, always with other.
function checkEntry(code, key, value) {
  if (typeof value === 'string') {
    if (value) assert.deepEqual(placeholders(value), placeholders(key), `${code}: placeholders differ: ${key}`);
    return !!value;
  }
  assert.ok(value && typeof value === 'object' && !Array.isArray(value), `${code}: not text or plural forms: ${key}`);
  assert.ok(placeholders(key).includes('n'), `${code}: plural forms need {n}: ${key}`);
  const categories = new Intl.PluralRules(code).resolvedOptions().pluralCategories;
  assert.equal(typeof value.other, 'string', `${code}: plural forms need other: ${key}`);
  for (const [category, form] of Object.entries(value)) {
    assert.ok(categories.includes(category), `${code}: ${category} is not a plural form of this language: ${key}`);
    assert.equal(typeof form, 'string', `${code}: ${key} ${category}`);
    // A form may leave out {n} ("one picture"), but never brings a placeholder of its own.
    if (form) assert.ok(placeholders(form).every(p => placeholders(key).includes(p)), `${code}: placeholders differ: ${key} ${category}`);
  }
  if (value.other) assert.deepEqual(placeholders(value.other), placeholders(key), `${code}: placeholders differ: ${key} other`);
  return !!value.other;
}

test('every table matches the code: no unused keys, matching placeholders, plural forms of its language', () => {
  const {used} = usedKeys();
  assert.ok(english, 'locales/en.json');
  // Chinese is the source text, so it has no table.
  assert.ok(!('zh-CN' in locales), 'zh-CN is the source text, not a table');
  for (const [code, table] of Object.entries(locales)) {
    assert.match(code, CODE);
    assert.equal(typeof table, 'object');
    assert.ok(typeof table['@name'] === 'string' && table['@name'], `${code}.json needs "@name", the language in itself`);
    let translated = 0;
    for (const [key, value] of Object.entries(table)) {
      if (key.startsWith('@')) { assert.equal(key, '@name', `${code}.json: unknown ${key}`); continue; }
      assert.ok(used.has(key) || backendKeys.has(key), `${code}.json has a key no code uses: ${key}`);
      if (checkEntry(code, key, value) && used.has(key)) translated++;
    }
    console.log(`${code} (${table['@name']}): ${translated}/${used.size} texts translated, ${used.size - translated} fall back to Chinese`);
    const backend = [...shownBackend()], done = backend.filter(key => table[key] && (typeof table[key] === 'string' || table[key].other));
    console.log(`${code} (${table['@name']}): backend ${done.length}/${backend.length} texts translated, ${backend.length - done.length} fall back to Chinese`);
  }
});

function page(language, languages = ['zh-CN'], tables = {en: english}) {
  const elements = {text: [{textContent: '设置'}, {textContent: '没有译文的字'}], attrs: [{dataset: {tAttr: 'title aria-label'}, attributes: {title: '任务', 'aria-label': '视图选项'},
    hasAttribute(name) { return name in this.attributes; }, getAttribute(name) { return this.attributes[name]; }, setAttribute(name, value) { this.attributes[name] = value; }}]};
  const document = {documentElement: {lang: 'zh-CN'}, querySelectorAll: selector => selector === '[data-t]' ? elements.text : elements.attrs};
  const names = Object.fromEntries(Object.keys(tables).map(code => [code, tables[code]['@name'] || code]));
  const context = {self: {ILLUSTASH_LANGUAGE: language, ILLUSTASH_LANGUAGES: names, ILLUSTASH_TABLES: tables}, navigator: {languages}, document};
  vm.createContext(context);
  vm.runInContext(read('i18n.js'), context);
  return {context, document, elements, t: (...args) => vm.runInContext('t', context)(...args), I18N: vm.runInContext('I18N', context)};
}

test('the language setting picks the table; system follows the first browser language', () => {
  for (const [language, languages, expected] of [['en', ['zh-CN'], 'en'], ['zh-CN', ['en-US'], 'zh-CN'], ['system', ['zh-TW', 'en'], 'zh-CN'],
    ['system', ['zh'], 'zh-CN'], ['system', ['en-GB', 'zh-CN'], 'en'], ['system', ['ja-JP'], 'en'], [undefined, ['fr'], 'en']]) {
    const {context, document} = page(language, languages);
    assert.equal(vm.runInContext('I18N.language', context), expected, `${language} ${languages}`);
    assert.equal(document.documentElement.lang, expected);
  }
});

test('more tables: system takes the first browser language that has one; the settings list names each in itself', () => {
  const tables = {en: {'@name': 'English', '设置': 'Settings'}, fr: {'@name': 'Français', '设置': 'Réglages'}, 'pt-BR': {'@name': 'Português (Brasil)'}};
  for (const [language, languages, expected] of [['system', ['fr-CA', 'en'], 'fr'], ['system', ['ja', 'fr'], 'fr'], ['system', ['ja', 'zh-CN', 'fr'], 'zh-CN'],
    ['system', ['pt-br'], 'pt-BR'], ['system', ['pt-PT'], 'pt-BR'], ['system', ['ja'], 'en'], ['fr', ['en'], 'fr'], ['de', ['fr'], 'fr']]) {
    assert.equal(page(language, languages, tables).I18N.language, expected, `${language} ${languages}`);
  }
  assert.equal(page('fr', [], tables).t('设置'), 'Réglages');
  assert.equal(page('system', ['fr'], {fr: tables.fr}).I18N.language, 'fr');
  // Without English, a language nobody translated stays Chinese.
  assert.equal(page('system', ['ja'], {fr: tables.fr}).I18N.language, 'zh-CN');
  assert.deepEqual(JSON.parse(JSON.stringify(page('system', [], tables).I18N.languages)),
    [['zh-CN', '简体中文'], ['en', 'English'], ['fr', 'Français'], ['pt-BR', 'Português (Brasil)']]);
});

test('plural forms follow the language and {n}; Chinese and plain entries are unchanged', () => {
  const tables = {en: {'{n} 张': {one: '{n} picture', other: '{n} pictures'}, '{n} 组': {one: 'one group', other: '{n} groups'}, '只有 other {n}': {other: 'only {n}'}},
    fr: {'{n} 张': {one: '{n} image', many: '{n} d’images', other: '{n} images'}}};
  const en = page('en', [], tables), fr = page('fr', [], tables), zh = page('zh-CN', [], tables);
  assert.equal(en.t('{n} 张', {n: 1}), '1 picture');
  assert.equal(en.t('{n} 张', {n: '1'}), '1 picture');
  for (const n of [0, 2, '1,234', '1.234', '1 234']) assert.equal(en.t('{n} 张', {n}), `${n} pictures`);
  assert.equal(en.t('{n} 张', {n: '<b>1</b>'}), '<b>1</b> pictures');
  assert.equal(en.t('{n} 张'), '{n} pictures');
  assert.equal(en.t('{n} 组', {n: 1}), 'one group');
  assert.equal(en.t('只有 other {n}', {n: 1}), 'only 1');
  assert.equal(fr.t('{n} 张', {n: 1}), '1 image');
  assert.equal(fr.t('{n} 张', {n: 0}), '0 image');
  assert.equal(fr.t('{n} 张', {n: 2}), '2 images');
  assert.equal(zh.t('{n} 张', {n: 1}), '1 张');
});

test('numbers and dates follow the page language; Chinese keeps its wording', () => {
  // The helpers as app.js has them, run against an I18N of each language.
  const source = readFileSync(path.join(STATIC, 'app.js'), 'utf8');
  const helpers = source.slice(source.indexOf('const number ='), source.indexOf('const work ='));
  const run = (language, code) => { const {context} = page(language, [], {en: english}); vm.runInContext(helpers + ';globalThis.out = ' + code, context); return context.out; };
  assert.equal(run('zh-CN', 'number(12345)'), '12,345');
  assert.equal(run('zh-CN', "dateText(2025, 5, 3, true) + '|' + dateText(2026, 5, 3) + '|' + dateText('2026', '05', '03') + '|' + monthText('2026', '05')"),
    '2025 年 5 月 3 日|5 月 3 日|05 月 03 日|2026 年 05 月');
  assert.equal(run('en', "number(12345) + '|' + dateText(2025, 5, 3, true) + '|' + dateText('2026', '05', '03') + '|' + monthText('2026', '05')"),
    '12,345|5/3/2025|5/3|May 2026');
});

test('t() translates, falls back to Chinese and fills placeholders', () => {
  const table = {'设置': 'Settings', '{n} 项进行中': '{n} running', '空译文': ''};
  const en = page('en', [], {en: table}), zh = page('zh-CN', [], {en: table});
  assert.equal(en.t('设置'), 'Settings');
  assert.equal(zh.t('设置'), '设置');
  assert.equal(en.t('没有译文'), '没有译文');
  assert.equal(en.t('空译文'), '空译文');
  assert.equal(en.t('{n} 项进行中', {n: 3}), '3 running');
  assert.equal(zh.t('{n} 项进行中', {n: 3}), '3 项进行中');
  assert.equal(en.t('{a} 和 {b}', {a: 1}), '1 和 {b}');
  assert.equal(en.t('toString'), 'toString');
  const both = {'移除': 'Remove', 'column\u0004移除': 'Removed'};
  assert.equal(page('en', [], {en: both}).t('移除'), 'Remove');
  assert.equal(page('en', [], {en: both}).t('移除', {context: 'column'}), 'Removed');
  assert.equal(page('zh-CN', [], {en: both}).t('移除', {context: 'column'}), '移除');
});

// What the page decides must not hang on the words it shows: on an English page a test against Chinese text never
// matches. Use a status field instead, or for a message the backend sends, said(text, '模板'): the backend's text
// stays Chinese in any page language and its template is its code, which must be a literal of the backend's source.
test('stylesheets show no Chinese of their own', () => {
  // CSS cannot call t(): generated text comes from an attribute the page fills, as content: attr(data-more).
  for (const name of readdirSync(STATIC).filter(n => n.endsWith('.css')))
    assert.doesNotMatch(read(name), /content:\s*["'][^"']*\p{Script=Han}/u, name);
});
test('no logic tests the Chinese text the page shows', () => {
  for (const name of scripts) {
    read(name).split('\n').forEach((line, i) => {
      if (/^\s*\/\//.test(line)) return;
      for (const m of line.matchAll(/\/((?:\\.|[^/\n])*\p{Script=Han}(?:\\.|[^/\n])*)\/[a-z]*\.test\(|\.(?:includes|startsWith|endsWith|indexOf)\(\s*(['"])([^'"]*\p{Script=Han}[^'"]*)\2/gu)) {
        assert.fail(`${name}:${i + 1} tests Chinese text: ${m[0]}`);
      }
    });
  }
});

test('the backend messages the page tests are templates of the backend', () => {
  let count = 0;
  for (const name of scripts) {
    const source = read(name);
    for (const call of source.matchAll(/(?<![\w.$])said\(/g)) {
      if (/^\s*\/\//.test(source.slice(source.lastIndexOf('\n', call.index) + 1, call.index))) continue;
      const literal = source.slice(call.index).match(/^said\([^,()]*(?:\([^()]*\))?[^,()]*,\s*(['"])((?:\\.|(?!\1)[^\\\n])*)\1\s*\)/);
      assert.ok(literal, `${name}:${source.slice(0, call.index).split('\n').length}: said() takes a quoted template`);
      const template = unescape(literal[1], literal[2]);
      assert.ok(backendKeys.has(template), `${name}: the backend has no such text: ${template}`);
      count++;
    }
  }
  assert.ok(count > 0);
});

test('backend text: an exact entry, else the most specific template; what placeholders held is translated too', () => {
  const table = {'同步失败：{reason}': 'Sync failed: {reason}', '文件夹不存在或无法访问：{path}': 'Folder missing or unreadable: {path}',
    '已将 {n} 张移出 {name}': {one: 'Removed {n} picture from {name}', other: 'Removed {n} pictures from {name}'},
    '已将 {n} 张移出 {name}（跳过 {skipped} 张）': 'Removed {n} from {name} ({skipped} skipped)', '{a}：{b}': '{a}: {b}',
    '其他': 'Other', '会话已过期，请刷新页面': 'Session expired, reload the page', 'join\u0004；': '; '};
  const en = page('en', [], {en: table}), zh = page('zh-CN', [], {en: table});
  const tb = text => vm.runInContext('tb', en.context)(text), said = (...a) => vm.runInContext('said', en.context)(...a);
  assert.equal(tb('其他'), 'Other');
  assert.equal(tb('会话已过期，请刷新页面'), 'Session expired, reload the page');
  assert.equal(tb('同步失败：文件夹不存在或无法访问：/合成/甲'), 'Sync failed: Folder missing or unreadable: /合成/甲');
  assert.equal(tb('已将 1 张移出 雪'), 'Removed 1 picture from 雪');
  assert.equal(tb('已将 3 张移出 雪'), 'Removed 3 pictures from 雪');
  assert.equal(tb('已将 3 张移出 雪（跳过 1 张）'), 'Removed 3 from 雪 (skipped 1)'.replace('skipped 1', '1 skipped'));
  // A template with no Chinese wording of its own never matches; text no template fits stays as sent.
  assert.equal(tb('甲：乙'), '甲：乙');
  // {n} only stands for a number.
  assert.equal(tb('已将 几 张移出 雪'), '已将 几 张移出 雪');
  assert.equal(tb('没有模板的话'), '没有模板的话');
  assert.equal(tb('Plain English'), 'Plain English');
  assert.equal(tb(null), null);
  assert.equal(vm.runInContext('tb', zh.context)('已将 3 张移出 雪'), '已将 3 张移出 雪');
  // said() reads the backend's text as sent, whatever the page's language.
  assert.ok(said('会话已过期，请刷新页面', '会话已过期，请刷新页面'));
  assert.ok(said('文件夹不存在或无法访问：/合成/甲', '文件夹不存在或无法访问：{path}'));
  assert.ok(said('同步失败：文件夹不存在或无法访问：/合成/甲', '文件夹不存在或无法访问：{path}'));
  assert.ok(!said('文件夹不存在或无法访问：/合成/甲', '同步失败：{reason}'));
  assert.ok(!said('不是：会话已过期，请刷新页面了', '会话已过期，请刷新页面'));
  // Messages joined with ；, each translated.
  assert.equal(tb('其他；已将 1 张移出 雪；没有模板'), 'Other; Removed 1 picture from 雪; 没有模板');
  // A message that has a ； of its own stays whole.
  const semi = page('en', [], {en: {...table, '甲；乙 {n} 张': 'A; B {n}', '丙 {n} 处': 'C {n}'}}).context;
  assert.equal(vm.runInContext('tb', semi)('丙 2 处；甲；乙 3 张'), 'C 2; A; B 3');
  assert.ok(!said(undefined, '其他'));
});

test('marked page text is translated only on a page in another language', () => {
  const table = {'设置': 'Settings', '任务': 'Tasks', '视图选项': 'View options'};
  const en = page('en', [], {en: table});
  assert.deepEqual(en.elements.text.map(e => e.textContent), ['Settings', '没有译文的字']);
  assert.deepEqual(en.elements.attrs[0].attributes, {title: 'Tasks', 'aria-label': 'View options'});
  const zh = page('zh-CN', [], {en: table});
  assert.deepEqual(zh.elements.text.map(e => e.textContent), ['设置', '没有译文的字']);
  assert.deepEqual(zh.elements.attrs[0].attributes, {title: '任务', 'aria-label': '视图选项'});
});

