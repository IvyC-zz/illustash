// Translation consistency (#173, #182): every t('…') literal and every data-t / data-t-attr text in the page matches
// each table in web/static/locales/<code>.json, which has no keys the code no longer uses, and whose placeholders,
// contexts and plural forms fit the Chinese. Text not yet translated falls back to Chinese: it is counted and reported
// per language, never a failure.
const {test} = require('node:test');
const assert = require('node:assert/strict');
const {readFileSync, readdirSync} = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const STATIC = path.join(__dirname, '../web/static');
const read = name => readFileSync(path.join(STATIC, name), 'utf8');
// The same rule as native/terms/src/app_preferences.rs: a language code names the file.
const CODE = /^[a-z]{2,3}(-[A-Za-z0-9]{1,8})*$/;
const locales = Object.fromEntries(readdirSync(path.join(STATIC, 'locales')).filter(name => name.endsWith('.json'))
  .map(name => [name.slice(0, -5), JSON.parse(read('locales/' + name))]));
const english = locales.en;
// i18n.js itself only translates text handed to it.
const scripts = readdirSync(STATIC).filter(name => name.endsWith('.js') && name !== 'i18n.js');
const placeholders = text => [...text.matchAll(/\{(\w+)\}/g)].map(m => m[1]).sort();
const unescape = (quote, body) => vm.runInNewContext(quote + body + quote);

function scriptKeys(name) {
  const source = read(name), keys = [], dynamic = [];
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

function usedKeys() {
  const used = new Set(), dynamic = [];
  for (const name of scripts) { const found = scriptKeys(name); found.keys.forEach(k => used.add(k)); dynamic.push(...found.dynamic); }
  markupKeys(read('index.html')).forEach(k => used.add(k));
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
      assert.ok(used.has(key), `${code}.json has a key no code uses: ${key}`);
      if (checkEntry(code, key, value)) translated++;
    }
    console.log(`${code} (${table['@name']}): ${translated}/${used.size} texts translated, ${used.size - translated} fall back to Chinese`);
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
// matches. Use a status field instead. Matching a message the backend sends is allowed only where listed, until the
// backend sends codes (#173 stage 3).
const BACKEND_MESSAGES = ['不存在或无法访问'];
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
        const text = m[1] ?? m[3];
        assert.ok(BACKEND_MESSAGES.includes(text), `${name}:${i + 1} tests Chinese text: ${m[0]}`);
      }
    });
  }
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

