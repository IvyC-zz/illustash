// The page's ES modules (web/static/core, web/static/ui; #199) in Node. They read t(), tb(), said() and I18N from the
// page's globals (web/static/i18n.js is still a classic script), here those of a Chinese page; fetch, document,
// localStorage and the like are the test's own: set them on globalThis before calling.
const path = require('node:path');
const {pathToFileURL} = require('node:url');
Object.assign(globalThis, require('./i18n_zh.cjs').page);
const load = name => import(pathToFileURL(path.join(__dirname, '../web/static', name)).href);
// core/ is loaded once per test file by ready() (pass it to node:test's before()); core() then gives each test what it
// exports with a new session and nothing to undo, also for a context that runs a slice of app.js not yet moved.
let loaded;
async function ready() {
  loaded = Object.assign({}, ...await Promise.all(['format', 'prefs', 'session', 'edits'].map(name => load(`core/${name}.js`))));
}
function core() {
  if (!loaded) throw new Error('call before(web.ready) first');
  Object.assign(loaded.session, {token: '', stale: false});
  Object.assign(loaded.edits, {latest: null, busy: false});
  return loaded;
}
module.exports = {load, ready, core};
