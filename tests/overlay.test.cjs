// 浮层管理器（ui/overlay.js，#343）：栈的顺序、Esc 只关栈顶一个、栈空时放行、点外关闭、点背景只对声明过的对话框、
// 模态对话框压住下面的层、大图让位给输入框、被藏起来或换掉的层自动清掉、焦点归还。
// 用普通对象拼成的页面：节点只有 parent / hidden / contains / closest / focus 这几样，够管理器用。
const {test, before, beforeEach} = require('node:test');
const assert = require('node:assert/strict');
const web = require('./web.cjs');
let overlay, doc, focused;
before(async () => { await web.ready(); overlay = await web.load('ui/overlay.js'); });

function node(name, parent = null, extra = {}) {
  const n = {name, parent, hidden: false, isConnected: true, typing: false, ...extra,
    contains(x) { for (let p = x; p; p = p.parent) if (p === n) return true; return false; },
    closest(selector) {
      for (let p = n; p; p = p.parent) if (selector === '[hidden]' ? p.hidden : p.typing) return p;
      return null;
    },
    focus() { doc.activeElement = n; focused.push(name); }};
  return n;
}
function dialogNode(name) {
  const d = node(name, null, {open: false, closed: 0, listeners: [],
    showModal() { d.open = true; }, close(value = '') { d.open = false; d.returnValue = value; d.closed++; for (const fn of d.listeners) fn(); },
    addEventListener(type, fn) { if (type === 'close') d.listeners.push(fn); },
    getBoundingClientRect: () => ({left: 10, right: 100, top: 10, bottom: 100})});
  return d;
}
beforeEach(() => {
  const handlers = {};
  focused = [];
  doc = {activeElement: null, body: node('body'), addEventListener(type, fn) { (handlers[type] ||= []).push(fn); },
    key(key, target = node('target')) { const e = {key, target, prevented: false, stopped: false, preventDefault() { this.prevented = true; }, stopImmediatePropagation() { this.stopped = true; }}; for (const fn of handlers.keydown || []) fn(e); return e; },
    click(target, extra = {}) { for (const fn of handlers.click || []) fn({target, clientX: 0, clientY: 0, ...extra}); }};
  globalThis.document = doc;
  overlay.resetLayers(); overlay.wireOverlays();
});

// 一个非模态浮层：元素 + 触发按钮，关闭就是 hidden = true。
function popover(name, extra = {}) {
  const button = node(name + '-button'), el = node(name), item = node(name + '-item', el);
  const layer = overlay.createLayer({kind: 'popover', id: name, el, anchor: button, onDismiss: reason => { el.hidden = true; layer.close(reason); }, ...extra});
  const open = () => { el.hidden = false; layer.open(); };
  return {button, el, item, layer, open, closeAs: reason => { el.hidden = true; layer.close(reason); }};
}

test('Esc closes only the top layer, in the reverse order they opened, and each key press is consumed', () => {
  const a = popover('view'), b = popover('task'); a.open(); b.open();
  assert.deepEqual(overlay.layerIds(), ['view', 'task']);
  const first = doc.key('Escape'); assert.equal(b.el.hidden, true); assert.equal(a.el.hidden, false); assert.equal(first.prevented && first.stopped, true);
  doc.key('Escape'); assert.equal(a.el.hidden, true); assert.deepEqual(overlay.layerIds(), []);
});
test('an empty stack lets Esc through to the page, and other keys never touch the stack', () => {
  const e = doc.key('Escape'); assert.equal(e.prevented, false); assert.equal(e.stopped, false);
  const a = popover('view'); a.open();
  const other = doc.key('Enter'); assert.equal(other.stopped, false); assert.equal(a.el.hidden, false);
});
test('a popover opened over a dialog closes first; the dialog is next (the search menu inside a form)', () => {
  const d = dialogNode('form'); overlay.openModal(d, {backdropClose: false});
  const menu = popover('menu'); menu.open();
  doc.key('Escape', node('input', d, {typing: true}));
  assert.equal(menu.el.hidden, true); assert.equal(d.open, true, 'Esc in the input only closes the menu');
  doc.key('Escape'); assert.equal(d.open, false);
});
test('a dialog opened over a popover closes first, as the task panel does with its confirmations', () => {
  const panel = popover('task'); panel.open();
  const d = dialogNode('confirm'); overlay.openModal(d, {backdropClose: true});
  doc.key('Escape'); assert.equal(d.open, false); assert.equal(panel.el.hidden, false);
  doc.key('Escape'); assert.equal(panel.el.hidden, true);
});
test('an overlay such as the tour sits where it was opened: a panel opened from it closes first', () => {
  const tour = node('tour'), layer = overlay.createLayer({kind: 'overlay', id: 'tour', el: tour, onDismiss: () => { tour.hidden = true; layer.close(); }});
  layer.open();
  const panel = popover('task'); panel.open();
  doc.key('Escape'); assert.equal(panel.el.hidden, true); assert.equal(tour.hidden, false);
  doc.key('Escape'); assert.equal(tour.hidden, true);
});
test('a click outside a popover and its button closes it; inside it, or on its button, does not', () => {
  const a = popover('tag'); a.open();
  doc.click(a.item); assert.equal(a.el.hidden, false, 'inside');
  doc.click(a.button); assert.equal(a.el.hidden, false, 'the button toggles it itself');
  doc.click(node('elsewhere')); assert.equal(a.el.hidden, true); assert.deepEqual(overlay.layerIds(), []);
});
test('one outside click closes every open popover it is outside of, and leaves the one it hit', () => {
  const a = popover('view'), b = popover('task'); a.open(); b.open();
  doc.click(b.item); assert.equal(a.el.hidden, true); assert.equal(b.el.hidden, false);
});
test('an overlay has no outside click; a dialog closes on its backdrop only when it said so', () => {
  const tour = node('tour'), layer = overlay.createLayer({kind: 'overlay', id: 'tour', el: tour}); layer.open();
  doc.click(node('elsewhere')); assert.equal(tour.hidden, false);
  layer.close();
  const info = dialogNode('help'), form = dialogNode('profile');
  overlay.openModal(info, {backdropClose: true}); doc.click(info, {clientX: 50, clientY: 50}); assert.equal(info.open, true, 'inside the box');
  doc.click(node('child', info)); assert.equal(info.open, true, 'a child is not the backdrop');
  doc.click(info); assert.equal(info.open, false);
  overlay.openModal(form); doc.click(form); assert.equal(form.open, true, 'the default keeps a form open');
  const explicit = dialogNode('explicit'); overlay.openModal(explicit, {backdropClose: false}); doc.click(explicit); assert.equal(explicit.open, true);
});
test('a dialog shields the layers under it from outside clicks', () => {
  const panel = popover('task'); panel.open();
  const d = dialogNode('index-add'); overlay.openModal(d, {backdropClose: false});
  doc.click(node('dialog-inside', d)); doc.click(d); assert.equal(panel.el.hidden, false);
  d.close(); doc.click(node('elsewhere')); assert.equal(panel.el.hidden, true);
});
test('the viewer lets a text field outside it take the Esc; a field inside it, or no field, does not', () => {
  const viewer = node('viewer'), layer = overlay.createLayer({kind: 'overlay', id: 'viewer', el: viewer, yieldToInput: true, onDismiss: () => { viewer.hidden = true; layer.close(); }});
  layer.open();
  const e = doc.key('Escape', node('search', null, {typing: true})); assert.equal(e.stopped, false); assert.equal(viewer.hidden, false);
  doc.key('Escape'); assert.equal(viewer.hidden, true);
  layer.open(); viewer.hidden = false;
  doc.key('Escape', node('inside', viewer, {typing: true})); assert.equal(viewer.hidden, true);
});
test('a popover stays ahead of a text field though: Esc in the search box closes the view options first', () => {
  const a = popover('view'); a.open();
  const e = doc.key('Escape', node('search', null, {typing: true})); assert.equal(a.el.hidden, true); assert.equal(e.stopped, true);
});
test('layers that were hidden, replaced or whose dialog closed are dropped without costing an Esc', () => {
  const a = popover('a'), b = popover('b'), c = popover('c'); a.open(); b.open(); c.open();
  c.el.hidden = true; b.el.isConnected = false;
  assert.deepEqual(overlay.layerIds(), ['a']);
  const d = dialogNode('d'); overlay.openModal(d); d.open = false; assert.deepEqual(overlay.layerIds(), ['a']);
  const parent = node('page'), child = popover('nested'); child.el.parent = parent; child.open(); parent.hidden = true;
  assert.deepEqual(overlay.layerIds(), ['a']);
  doc.key('Escape'); assert.equal(a.el.hidden, true);
});
test('the element can be a function, for menus that are drawn again each time', () => {
  let el = node('first');
  const layer = overlay.createLayer({kind: 'popover', id: 'cast', el: () => el, onDismiss: () => { el.hidden = true; layer.close(); }});
  layer.open(); el = node('second');
  doc.click(el); assert.equal(el.hidden, false, 'inside the current element');
  doc.click(node('elsewhere')); assert.equal(el.hidden, true);
});
test('open and close are idempotent, and a dialog leaves the stack when it closes by any route', () => {
  const a = popover('a'); a.open(); a.open(); assert.deepEqual(overlay.layerIds(), ['a']);
  a.layer.close(); a.layer.close(); assert.deepEqual(overlay.layerIds(), []);
  const d = dialogNode('d'), layer = overlay.openModal(d); assert.equal(overlay.openModal(d), layer, 'the same dialog is one layer');
  d.close('ok'); assert.deepEqual(overlay.layerIds(), []);
  assert.equal(d.closed, 1);
});
test('focus goes back to where it was when Esc or a pick closes a popover, not after an outside click', () => {
  const opener = node('opener'); doc.activeElement = opener;
  const a = popover('menu'); a.open();
  const field = node('field', a.el); doc.activeElement = field;
  doc.key('Escape'); assert.deepEqual(focused, ['opener']);
  doc.activeElement = opener; a.open(); doc.activeElement = field; a.closeAs('api'); assert.deepEqual(focused, ['opener', 'opener']);
  doc.activeElement = opener; a.open(); doc.activeElement = field; doc.click(node('elsewhere')); assert.deepEqual(focused, ['opener', 'opener'], 'outside click: no focus move');
  doc.activeElement = opener; a.open(); doc.activeElement = field; a.closeAs('silent'); assert.equal(focused.length, 2, 'a redraw does not grab the focus');
});
test('focus is not taken from somewhere else, and the trigger button is the fallback', () => {
  const a = popover('menu'); doc.activeElement = doc.body; a.open();
  const elsewhere = node('elsewhere'); doc.activeElement = elsewhere; a.closeAs('escape'); assert.deepEqual(focused, []);
  doc.activeElement = doc.body; a.open(); doc.activeElement = doc.body; a.closeAs('escape'); assert.deepEqual(focused, ['menu-button']);
});
test('dismissTop is the same as Esc and says whether anything was there', () => {
  assert.equal(overlay.dismissTop(), false);
  const a = popover('a'); a.open(); assert.equal(overlay.dismissTop(), true); assert.equal(a.el.hidden, true);
});
