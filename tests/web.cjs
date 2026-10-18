// The page's ES modules (web/static/core, web/static/ui; #199) in Node. web/static/i18n.js, the classic script that
// runs before them on the page, runs here first as a script of this process, as the page has it: a Chinese page
// unless language() picks another before ready(); one language per test file. fetch, document, localStorage and the
// like are the test's own: set them on globalThis before calling.
const path = require('node:path');
const vm = require('node:vm');
const {readFileSync} = require('node:fs');
const {pathToFileURL} = require('node:url');
const STATIC = path.join(__dirname, '../web/static');
const load = name => import(pathToFileURL(path.join(STATIC, name)).href);
let page = {code: 'zh-CN', tables: {}}, loaded;
// A page in this language, with these tables ({code: table}; English from locales/en.json by default).
function language(code, tables = {en: JSON.parse(readFileSync(path.join(STATIC, 'locales/en.json'), 'utf8'))}) {
  if (loaded) throw new Error('language() comes before ready()');
  page = {code, tables};
}
// Runs i18n.js and loads core/ once per test file: pass it to node:test's before().
async function ready() {
  const names = Object.fromEntries(Object.entries(page.tables).map(([c, table]) => [c, table['@name'] || c]));
  globalThis.self = {ILLUSTASH_LANGUAGE: page.code, ILLUSTASH_LANGUAGES: names, ILLUSTASH_TABLES: page.tables};
  globalThis.document = {documentElement: {}, querySelectorAll: () => []};
  vm.runInThisContext(readFileSync(path.join(STATIC, 'i18n.js'), 'utf8'), {filename: path.join(STATIC, 'i18n.js')});
  loaded = Object.assign({}, ...await Promise.all(['i18n', 'format', 'prefs', 'session', 'edits', 'state', 'labels', 'jobs', 'settings', 'meta', 'route'].map(name => load(`core/${name}.js`))));
}
// What core/ exports, with a new session and nothing to undo. The rest of core/state.js is the test's to set.
function core() {
  if (!loaded) throw new Error('call before(web.ready) first');
  Object.assign(loaded.session, {token: '', stale: false});
  Object.assign(loaded.edits, {latest: null, busy: false});
  return loaded;
}

// A page made of plain objects, enough for the desktop layout's modules to draw into (#199): getElementById() makes an
// element on first use; elements keep children, style, dataset, classes and listeners, and innerHTML as text.
// Returns {document, el(id)}; also sets globalThis.document.
class FakeElement {
  constructor(tag = 'div', id = '') {
    Object.assign(this, {tagName: tag.toUpperCase(), id, hidden: false, textContent: '', innerHTML: '', value: '', title: '', className: '',
      style: {}, dataset: {}, children: [], parent: null, listeners: {}, attributes: {}, scrollTop: 0, clientWidth: 1000, clientHeight: 800});
    const classes = new Set();
    this.classList = {add: (...c) => c.forEach(x => classes.add(x)), remove: (...c) => c.forEach(x => classes.delete(x)),
      toggle: (c, on = !classes.has(c)) => (on ? classes.add(c) : classes.delete(c), on), contains: c => classes.has(c)};
  }
  get firstElementChild() { return this.children[0] || null; }
  get nextElementSibling() { return this.parent?.children[this.parent.children.indexOf(this) + 1] || null; }
  append(...els) { for (const el of els) if (typeof el === 'object') this.insertBefore(el, null); }
  insertBefore(el, before) {
    el.remove?.(); const i = before ? this.children.indexOf(before) : this.children.length;
    this.children.splice(i < 0 ? this.children.length : i, 0, el); el.parent = this; return el;
  }
  insertAdjacentHTML(where, html) { this.innerHTML += html; }
  remove() { if (this.parent) { const a = this.parent.children; a.splice(a.indexOf(this), 1); this.parent = null; } }
  replaceWith(el) { const p = this.parent; if (p) { p.insertBefore(el, this); this.remove(); } }
  addEventListener(type, fn) { (this.listeners[type] ||= []).push(fn); }
  removeEventListener() {}
  setAttribute(k, v) { this.attributes[k] = String(v); }
  getAttribute(k) { return this.attributes[k] ?? null; }
  hasAttribute(k) { return k in this.attributes; }
  removeAttribute(k) { delete this.attributes[k]; }
  querySelector() { return new FakeElement(); }
  querySelectorAll() { return []; }
  closest() { return null; }
  contains(el) { return el === this; }
  matches() { return false; }
  focus() {} blur() {} scrollIntoView() {} showModal() { this.open = true; } close() { this.open = false; } click() {}
  getBoundingClientRect() { return {left: 0, top: 0, right: this.clientWidth, bottom: this.clientHeight, width: this.clientWidth, height: this.clientHeight}; }
}
function fakePage() {
  const nodes = new Map();
  const document = {
    getElementById: id => { if (!nodes.has(id)) nodes.set(id, new FakeElement('div', id)); return nodes.get(id); },
    createElement: tag => tag === 'template' ? {set innerHTML(html) { this.content = {firstElementChild: Object.assign(new FakeElement(), {html})}; }} : new FakeElement(tag),
    querySelector: () => null, querySelectorAll: () => [], addEventListener() {}, activeElement: null,
    body: new FakeElement('body'), documentElement: new FakeElement('html'),
  };
  globalThis.document = document;
  return {document, el: id => document.getElementById(id)};
}
module.exports = {load, language, ready, core, fakePage, FakeElement};
