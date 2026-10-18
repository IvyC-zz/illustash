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
  loaded = Object.assign({}, ...await Promise.all(['i18n', 'format', 'prefs', 'session', 'edits'].map(name => load(`core/${name}.js`))));
}
// What core/ exports, with a new session and nothing to undo, also for a context that runs a slice of app.js not
// yet moved.
function core() {
  if (!loaded) throw new Error('call before(web.ready) first');
  Object.assign(loaded.session, {token: '', stale: false});
  Object.assign(loaded.edits, {latest: null, busy: false});
  return loaded;
}
module.exports = {load, language, ready, core};
