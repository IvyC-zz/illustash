const {test} = require('node:test');
const assert = require('node:assert/strict');
const vm = require('node:vm');
const source = require('node:fs').readFileSync(require('node:path').join(__dirname, '../web/static/app.js'), 'utf8');
const tick = () => new Promise(resolve => setImmediate(resolve));

function harness() {
  const dialogs = [], calls = [], consent = new Map();
  let click;
  const document = {body: {append(d) { dialogs.push(d); }}, createElement() {
    const nodes = {};
    return {returnValue: '', setAttribute() {}, querySelector(selector) {return nodes[selector] ||= {};},
      showModal() {this.open = true;}, close(value = '') {this.returnValue = value; this.open = false; this.onclose();},
      remove() {this.removed = true;}, getBoundingClientRect: () => ({left:10,right:100,top:10,bottom:100})};
  }};
  const c = {document, state: {token:'synthetic-token',route:{page:'settings'}}, removingSources: new Set(), confirm() {throw new Error('must not use plugin confirm');}, number: String,
    localStorage: {getItem: k => consent.get(k), setItem: (k,v) => consent.set(k,v)},
    $: id => id === 'settingsPage' ? {addEventListener(event, callback) {click = callback;}} : {contains: () => true},
    libraryAction: async (action, body) => {calls.push({action, body}); return body.confirm ? {removed:2} : {count:2};},
    loadLibraries: async () => {}, toast() {}, pollSync() {},
    api: async (path, options) => {calls.push({path, options});}, clearTimeout() {}, setTimeout() {}};
  vm.createContext(c);
  vm.runInContext(source.slice(source.indexOf('let confirmationOpen ='), source.indexOf('// Native folder picker')),c);
  vm.runInContext(source.slice(source.indexOf("$('settingsPage').addEventListener('click', async e =>"),source.indexOf('async function openLibrary(')),c);
  vm.runInContext(source.slice(source.indexOf('const LOOKUP_NOTE ='),source.indexOf('const findArtists =')),c);
  const remove = () => click({target:{closest:()=>({dataset:{libRemove:'synthetic-source'}})}});
  return {c, dialogs, calls, consent, remove};
}

test('source removal waits with N images visible and submits only after explicit confirmation', async () => {
  const h = harness(), pending = h.remove(); await tick();
  assert.equal(h.calls.length,1);
  const d = h.dialogs[0]; assert.equal(d.open,true);
  assert.match(d.querySelector('p').textContent,/2 张图/);
  d.close('ok'); await pending;
  assert.equal(h.calls.length,2);
  assert.equal(h.calls[1].body.confirm,1); assert.equal(d.removed,true);
});

for (const dismiss of ['cancel','close','escape','backdrop']) test(`${dismiss} leaves source removal unsubmitted`, async () => {
  const h = harness(), pending = h.remove(); await tick(); const d = h.dialogs[0];
  if (dismiss === 'close') d.querySelector('[data-close]').onclick();
  else if (dismiss === 'backdrop') d.onclick({target:d,clientX:0,clientY:0});
  else d.close(dismiss === 'cancel' ? 'cancel' : '');
  await pending; assert.equal(h.calls.length,1); assert.equal(d.removed,true);
});

test('artist lookup waits for authorization and cancellation saves no consent or request', async () => {
  const h = harness(); const pending = h.c.artistLookup({action:'lookup',ids:['synthetic']},'lookup');
  await tick(); assert.equal(h.calls.length,0); assert.equal(h.consent.size,0);
  h.dialogs[0].close(); await pending;
  assert.equal(h.calls.length,0); assert.equal(h.consent.size,0);
  const accepted = h.c.artistLookup({action:'lookup',ids:['synthetic']},'lookup');
  await tick(); assert.equal(h.calls.length,0);
  h.dialogs[1].close('ok'); await accepted;
  assert.equal(h.consent.get('artistLookupOk'),'1'); assert.equal(h.calls.length,1);
  await h.c.artistLookup({action:'artist',artist:'synthetic'},'lookup');
  assert.equal(h.dialogs.length,2); assert.equal(h.calls.length,2);
});


test('rapid repeated removal has one preview, dialog and commit', async () => {
  const h = harness(), first = h.remove(), second = h.remove(); await tick();
  assert.equal(h.calls.length,1); assert.equal(h.dialogs.length,1);
  h.dialogs[0].close('ok'); await Promise.all([first,second]);
  assert.equal(h.calls.length,2);
});
for (const change of ['token','route']) test(`confirmation after ${change} change cannot submit`, async () => {
  const h = harness(), pending = h.remove(); await tick();
  if (change === 'token') h.c.state.token = 'replacement-session';
  else h.c.state.route = {page:'home'};
  h.dialogs[0].close('ok'); await pending;
  assert.equal(h.calls.length,1);
});
