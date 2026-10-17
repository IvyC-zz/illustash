// Translation consistency (#173): every t('…') literal and every data-t / data-t-attr text in the page matches
// web/static/locales/en.json, which has no keys the code no longer uses, and whose placeholders match the Chinese.
// Text not yet translated falls back to Chinese: it is counted and reported, never a failure.
const {test} = require('node:test');
const assert = require('node:assert/strict');
const {readFileSync, readdirSync} = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const STATIC = path.join(__dirname, '../web/static');
const read = name => readFileSync(path.join(STATIC, name), 'utf8');
const english = JSON.parse(read('locales/en.json'));
// i18n.js itself only translates text handed to it.
const scripts = readdirSync(STATIC).filter(name => name.endsWith('.js') && name !== 'i18n.js');
const placeholders = text => [...text.matchAll(/\{(\w+)\}/g)].map(m => m[1]).sort();
const unescape = (quote, body) => vm.runInNewContext(quote + body + quote);

function scriptKeys(name) {
  const source = read(name), keys = [], dynamic = [];
  for (const call of source.matchAll(/(?<![\w.$])t\(\s*(['"`]?)/g)) {
    const rest = source.slice(call.index + call[0].length - call[1].length);
    const literal = call[1] && rest.match(/^(['"])((?:\\.|(?!\1)[^\\\n])*)\1\s*[,)]/);
    if (literal) keys.push(unescape(literal[1], literal[2]));
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

test('every translated text in the code is a plain literal with a matching table entry', () => {
  const used = new Set(), dynamic = [];
  for (const name of scripts) { const found = scriptKeys(name); found.keys.forEach(k => used.add(k)); dynamic.push(...found.dynamic); }
  markupKeys(read('index.html')).forEach(k => used.add(k));
  // A key computed at run time cannot be checked or collected for translators.
  assert.deepEqual(dynamic, [], 't() must be called with a single- or double-quoted literal');
  assert.ok(used.size > 0);
  for (const [key, value] of Object.entries(english)) {
    assert.ok(used.has(key), `en.json has a key no code uses: ${key}`);
    assert.equal(typeof value, 'string', key);
    if (value) assert.deepEqual(placeholders(value), placeholders(key), `placeholders differ: ${key}`);
  }
  const missing = [...used].filter(key => !english[key]);
  console.log(`English: ${used.size - missing.length}/${used.size} texts translated, ${missing.length} fall back to Chinese`);
});

function page(language, languages = ['zh-CN'], table = english) {
  const elements = {text: [{textContent: '设置'}, {textContent: '没有译文的字'}], attrs: [{dataset: {tAttr: 'title aria-label'}, attributes: {title: '任务', 'aria-label': '视图选项'},
    hasAttribute(name) { return name in this.attributes; }, getAttribute(name) { return this.attributes[name]; }, setAttribute(name, value) { this.attributes[name] = value; }}]};
  const document = {documentElement: {lang: 'zh-CN'}, querySelectorAll: selector => selector === '[data-t]' ? elements.text : elements.attrs};
  const context = {self: {ILLUSTASH_LANGUAGE: language, ILLUSTASH_EN: table}, navigator: {languages}, document};
  vm.createContext(context);
  vm.runInContext(read('i18n.js'), context);
  return {context, document, elements, t: (...args) => vm.runInContext('t', context)(...args)};
}

test('the language setting picks the table; system follows the first browser language', () => {
  for (const [language, languages, expected] of [['en', ['zh-CN'], 'en'], ['zh-CN', ['en-US'], 'zh-CN'], ['system', ['zh-TW', 'en'], 'zh-CN'],
    ['system', ['zh'], 'zh-CN'], ['system', ['en-GB', 'zh-CN'], 'en'], ['system', ['ja-JP'], 'en'], [undefined, ['fr'], 'en']]) {
    const {context, document} = page(language, languages);
    assert.equal(vm.runInContext('I18N.language', context), expected, `${language} ${languages}`);
    assert.equal(document.documentElement.lang, expected);
  }
});

test('t() translates, falls back to Chinese and fills placeholders', () => {
  const table = {'设置': 'Settings', '{n} 项进行中': '{n} running', '空译文': ''};
  const en = page('en', [], table), zh = page('zh-CN', [], table);
  assert.equal(en.t('设置'), 'Settings');
  assert.equal(zh.t('设置'), '设置');
  assert.equal(en.t('没有译文'), '没有译文');
  assert.equal(en.t('空译文'), '空译文');
  assert.equal(en.t('{n} 项进行中', {n: 3}), '3 running');
  assert.equal(zh.t('{n} 项进行中', {n: 3}), '3 项进行中');
  assert.equal(en.t('{a} 和 {b}', {a: 1}), '1 和 {b}');
  assert.equal(en.t('toString'), 'toString');
});

test('marked page text is translated only on an English page', () => {
  const table = {'设置': 'Settings', '任务': 'Tasks', '视图选项': 'View options'};
  const en = page('en', [], table);
  assert.deepEqual(en.elements.text.map(e => e.textContent), ['Settings', '没有译文的字']);
  assert.deepEqual(en.elements.attrs[0].attributes, {title: 'Tasks', 'aria-label': 'View options'});
  const zh = page('zh-CN', [], table);
  assert.deepEqual(zh.elements.text.map(e => e.textContent), ['设置', '没有译文的字']);
  assert.deepEqual(zh.elements.attrs[0].attributes, {title: '任务', 'aria-label': '视图选项'});
});

