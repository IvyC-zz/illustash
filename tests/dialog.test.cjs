// openDialog (ui/dialog.js, #319): the one builder of the page's runtime modal dialogs. Closing paths (×/[data-close],
// Esc, backdrop), the backdrop opt-out for forms with typed input, onClose before removal, and the focus hook.
const {test, before, afterEach} = require('node:test');
const assert = require('node:assert/strict');
const web = require('./web.cjs');
let dialog;
before(async () => { await web.ready(); dialog = await web.load('ui/dialog.js'); });
afterEach(() => { delete globalThis.document; });

function page() {
  const dialogs = [], focused = [];
  const field = {focus() { focused.push('field'); }};
  globalThis.document = {body: {append(d) { dialogs.push(d); }}, createElement() {
    const nodes = {'#name': field};
    return {returnValue: '', attributes: {}, setAttribute(k, v) { this.attributes[k] = v; }, querySelector(selector) { return nodes[selector] ||= {}; },
      showModal() { this.open = true; }, close(value = '') { this.returnValue = value; this.open = false; this.onclose(); },
      remove() { this.removed = true; }, getBoundingClientRect: () => ({left: 10, right: 100, top: 10, bottom: 100})};
  }};
  return {dialogs, focused};
}
const inside = {clientX: 50, clientY: 50}, outside = {clientX: 0, clientY: 0};

test('builds a titled modal with the shared class, aria-label and shell', () => {
  const {dialogs} = page(), d = dialog.openDialog({title: '标题', cls: 'work-dialog', body: '<p>正文</p>', actions: '<button class="primary">好</button>'});
  assert.equal(dialogs[0], d); assert.equal(d.open, true);
  assert.equal(d.className, 'modal work-dialog'); assert.equal(d.attributes['aria-label'], '标题');
  assert.equal(d.querySelector('h2').textContent, '标题');
  assert.match(d.innerHTML, /<form method="dialog">/); assert.match(d.innerHTML, /<p>正文<\/p>/);
  assert.match(d.innerHTML, /<div class="dialog-actions"><button class="primary">好<\/button><\/div>/);
  assert.match(d.innerHTML, /class="icon" data-close/);
});
test('without actions there is no action row', () => {
  page(); assert.doesNotMatch(dialog.openDialog({title: 'x'}).innerHTML, /dialog-actions/);
});
test('a click on the backdrop closes it; one inside the box or on the backdrop of a form that opted out does not', () => {
  page();
  const d = dialog.openDialog({title: 'x'});
  d.onclick({target: {closest: () => null}, ...outside}); assert.equal(d.open, true, 'a click on a child is not a backdrop click');
  d.onclick({target: d, ...inside}); assert.equal(d.open, true, 'inside the box');
  d.onclick({target: d, ...outside}); assert.equal(d.open, false); assert.equal(d.removed, true);
  const form = dialog.openDialog({title: 'x', backdropClose: false});
  form.onclick({target: form, ...outside}); assert.equal(form.open, true, 'a form with typed input stays');
});
test('any [data-close] inside closes it, also one drawn later', () => {
  page();
  const d = dialog.openDialog({title: 'x'});
  d.onclick({target: {closest: selector => (selector === '[data-close]' ? {} : null)}});
  assert.equal(d.open, false);
});
test('onClose sees the return value before the dialog is removed, and the dialog is removed even if it throws', () => {
  page();
  let seen;
  const d = dialog.openDialog({title: 'x', onClose: dlg => { seen = {value: dlg.returnValue, removed: dlg.removed}; }});
  d.close('ok'); assert.deepEqual(seen, {value: 'ok', removed: undefined}); assert.equal(d.removed, true);
  const bad = dialog.openDialog({title: 'x', onClose: () => { throw new Error('boom'); }});
  assert.throws(() => bad.close(), /boom/); assert.equal(bad.removed, true);
});
test('focus hands the focus to the named control', () => {
  const {focused} = page();
  dialog.openDialog({title: 'x'}); assert.deepEqual(focused, []);
  dialog.openDialog({title: 'x', focus: '#name'}); assert.deepEqual(focused, ['field']);
});
test('isBackdropClick is true only for the dialog itself outside its box', () => {
  const d = {getBoundingClientRect: () => ({left: 10, right: 100, top: 10, bottom: 100})};
  assert.equal(dialog.isBackdropClick(d, {target: d, ...outside}), true);
  assert.equal(dialog.isBackdropClick(d, {target: d, ...inside}), false);
  assert.equal(dialog.isBackdropClick(d, {target: {}, ...outside}), false);
});
