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

test('Esc 只关栈顶一个，按打开的相反顺序，这次按键被消费掉', () => {
  const a = popover('view'), b = popover('task'); a.open(); b.open();
  assert.deepEqual(overlay.layerIds(), ['view', 'task']);
  const first = doc.key('Escape'); assert.equal(b.el.hidden, true); assert.equal(a.el.hidden, false); assert.equal(first.prevented && first.stopped, true);
  doc.key('Escape'); assert.equal(a.el.hidden, true); assert.deepEqual(overlay.layerIds(), []);
});
test('栈空时 Esc 放行给页面，其他按键不碰栈', () => {
  const e = doc.key('Escape'); assert.equal(e.prevented, false); assert.equal(e.stopped, false);
  const a = popover('view'); a.open();
  const other = doc.key('Enter'); assert.equal(other.stopped, false); assert.equal(a.el.hidden, false);
});
test('对话框上面开的非模态浮层先关，再轮到对话框（表单里的搜索候选菜单）', () => {
  const d = dialogNode('form'); overlay.openModal(d, {backdropClose: false});
  const menu = popover('menu'); menu.open();
  doc.key('Escape', node('input', d, {typing: true}));
  assert.equal(menu.el.hidden, true); assert.equal(d.open, true, 'Esc in the input only closes the menu');
  doc.key('Escape'); assert.equal(d.open, false);
});
test('非模态浮层上面开的对话框先关（任务面板上的确认框）', () => {
  const panel = popover('task'); panel.open();
  const d = dialogNode('confirm'); overlay.openModal(d, {backdropClose: true});
  doc.key('Escape'); assert.equal(d.open, false); assert.equal(panel.el.hidden, false);
  doc.key('Escape'); assert.equal(panel.el.hidden, true);
});
test('导览这类覆盖层按打开的先后排：从它上面开的面板先关', () => {
  const tour = node('tour'), layer = overlay.createLayer({kind: 'overlay', id: 'tour', el: tour, onDismiss: () => { tour.hidden = true; layer.close(); }});
  layer.open();
  const panel = popover('task'); panel.open();
  doc.key('Escape'); assert.equal(panel.el.hidden, true); assert.equal(tour.hidden, false);
  doc.key('Escape'); assert.equal(tour.hidden, true);
});
test('点非模态浮层和它的触发按钮之外会关；点里面或点按钮不关', () => {
  const a = popover('tag'); a.open();
  doc.click(a.item); assert.equal(a.el.hidden, false, 'inside');
  doc.click(a.button); assert.equal(a.el.hidden, false, 'the button toggles it itself');
  doc.click(node('elsewhere')); assert.equal(a.el.hidden, true); assert.deepEqual(overlay.layerIds(), []);
});
test('一次点外同时关掉所有它在外面的非模态浮层，点中的那个不关', () => {
  const a = popover('view'), b = popover('task'); a.open(); b.open();
  doc.click(b.item); assert.equal(a.el.hidden, true); assert.equal(b.el.hidden, false);
});
test('覆盖层没有点外关闭；对话框只在声明过时才点背景关闭', () => {
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
test('对话框压住它下面的层，不响应点外关闭', () => {
  const panel = popover('task'); panel.open();
  const d = dialogNode('index-add'); overlay.openModal(d, {backdropClose: false});
  doc.click(node('dialog-inside', d)); doc.click(d); assert.equal(panel.el.hidden, false);
  d.close(); doc.click(node('elsewhere')); assert.equal(panel.el.hidden, true);
});
test('大图让位给它之外的输入框；框在它里面或没有输入框时不让位', () => {
  const viewer = node('viewer'), layer = overlay.createLayer({kind: 'overlay', id: 'viewer', el: viewer, yieldToInput: true, onDismiss: () => { viewer.hidden = true; layer.close(); }});
  layer.open();
  const e = doc.key('Escape', node('search', null, {typing: true})); assert.equal(e.stopped, false); assert.equal(viewer.hidden, false);
  doc.key('Escape'); assert.equal(viewer.hidden, true);
  layer.open(); viewer.hidden = false;
  doc.key('Escape', node('inside', viewer, {typing: true})); assert.equal(viewer.hidden, true);
});
test('非模态浮层不让位：搜索框里按 Esc 先关视图选项', () => {
  const a = popover('view'); a.open();
  const e = doc.key('Escape', node('search', null, {typing: true})); assert.equal(a.el.hidden, true); assert.equal(e.stopped, true);
});
test('被藏起来、换掉或对话框已关的层自动清出，不白占一次 Esc', () => {
  const a = popover('a'), b = popover('b'), c = popover('c'); a.open(); b.open(); c.open();
  c.el.hidden = true; b.el.isConnected = false;
  assert.deepEqual(overlay.layerIds(), ['a']);
  const d = dialogNode('d'); overlay.openModal(d); d.open = false; assert.deepEqual(overlay.layerIds(), ['a']);
  const parent = node('page'), child = popover('nested'); child.el.parent = parent; child.open(); parent.hidden = true;
  assert.deepEqual(overlay.layerIds(), ['a']);
  doc.key('Escape'); assert.equal(a.el.hidden, true);
});
test('元素可以是函数，给每次重画的菜单用', () => {
  let el = node('first');
  const layer = overlay.createLayer({kind: 'popover', id: 'cast', el: () => el, onDismiss: () => { el.hidden = true; layer.close(); }});
  layer.open(); el = node('second');
  doc.click(el); assert.equal(el.hidden, false, 'inside the current element');
  doc.click(node('elsewhere')); assert.equal(el.hidden, true);
});
test('open 和 close 幂等；对话框用任何途径关闭都出栈', () => {
  const a = popover('a'); a.open(); a.open(); assert.deepEqual(overlay.layerIds(), ['a']);
  a.layer.close(); a.layer.close(); assert.deepEqual(overlay.layerIds(), []);
  const d = dialogNode('d'), layer = overlay.openModal(d); assert.equal(overlay.openModal(d), layer, 'the same dialog is one layer');
  d.close('ok'); assert.deepEqual(overlay.layerIds(), []);
  assert.equal(d.closed, 1);
});
test('Esc 或选一项关闭时焦点归还，点外关闭不动焦点', () => {
  const opener = node('opener'); doc.activeElement = opener;
  const a = popover('menu'); a.open();
  const field = node('field', a.el); doc.activeElement = field;
  doc.key('Escape'); assert.deepEqual(focused, ['opener']);
  doc.activeElement = opener; a.open(); doc.activeElement = field; a.closeAs('api'); assert.deepEqual(focused, ['opener', 'opener']);
  doc.activeElement = opener; a.open(); doc.activeElement = field; doc.click(node('elsewhere')); assert.deepEqual(focused, ['opener', 'opener'], 'outside click: no focus move');
  doc.activeElement = opener; a.open(); doc.activeElement = field; a.closeAs('silent'); assert.equal(focused.length, 2, 'a redraw does not grab the focus');
});
test('不从别处抢焦点；没有打开者时回触发按钮', () => {
  const a = popover('menu'); doc.activeElement = doc.body; a.open();
  const elsewhere = node('elsewhere'); doc.activeElement = elsewhere; a.closeAs('escape'); assert.deepEqual(focused, []);
  doc.activeElement = doc.body; a.open(); doc.activeElement = doc.body; a.closeAs('escape'); assert.deepEqual(focused, ['menu-button']);
});
test('dismissTop 与 Esc 相同，并返回栈里有没有层', () => {
  assert.equal(overlay.dismissTop(), false);
  const a = popover('a'); a.open(); assert.equal(overlay.dismissTop(), true); assert.equal(a.el.hidden, true);
});

// #422：焦点移进层、菜单里的方向键、盖满页面的层把下面设成 inert、onDismiss 可以不关。
function menu(name, n = 3, extra = {}) {
  const p = popover(name, extra), list = Array.from({length: n}, (_, i) => node(`${name}-${i}`, p.el, {matches: () => false}));
  p.el.querySelectorAll = () => list;
  return {...p, list};
}
test('focusIn 把焦点移进层：first 是第一项，self 是层自己；关闭后还给打开它的按钮', () => {
  const m = menu('view', 3, {focusIn: 'first', menu: true});
  m.button.focus(); m.open();
  assert.equal(doc.activeElement, m.list[0]);
  doc.key('Escape', m.list[0]);
  assert.equal(doc.activeElement, m.button);
  const t = popover('tasks', {focusIn: 'self'}); t.el.querySelectorAll = () => [];
  t.button.focus(); t.open();
  assert.equal(doc.activeElement, t.el);
  doc.key('Escape', t.el); assert.equal(doc.activeElement, t.button);
});
test('菜单里 ↑ ↓ 循环移动、Home End 到两头，按键被消费；不是菜单的浮层不管方向键', () => {
  const m = menu('more', 3, {menu: true}); m.open(); m.list[0].focus();
  const down = doc.key('ArrowDown', m.list[0]);
  assert.equal(doc.activeElement, m.list[1]); assert.equal(down.prevented && down.stopped, true);
  doc.key('ArrowUp', m.list[1]); doc.key('ArrowUp', m.list[0]);
  assert.equal(doc.activeElement, m.list[2], 'wraps around');
  doc.key('Home', m.list[2]); assert.equal(doc.activeElement, m.list[0]);
  doc.key('End', m.list[0]); assert.equal(doc.activeElement, m.list[2]);
  // 焦点在菜单外（还在按钮上）：↓ 进第一项；在菜单外的输入框里：方向键归输入框
  doc.key('ArrowDown', m.button); assert.equal(doc.activeElement, m.list[0]);
  const typed = doc.key('ArrowDown', node('search', null, {typing: true})); assert.equal(typed.stopped, false);
  m.closeAs('api');
  const panel = popover('tasks'); panel.open();
  const free = doc.key('ArrowDown', panel.item); assert.equal(free.stopped, false);
  assert.equal(overlay.popoverOpen(), true); assert.equal(overlay.inPopover(panel.item), true); assert.equal(overlay.inPopover(node('grid')), false);
});
test('盖满页面的层把声明的元素设成 inert，关掉后恢复；它上面再开的层所在的元素不设', () => {
  const app = node('app'), bar = node('bar'), tasks = node('tasks-root'), viewer = node('viewer');
  const layer = overlay.createLayer({kind: 'overlay', id: 'viewer', el: viewer, inert: () => [app, bar, tasks]});
  layer.open();
  assert.deepEqual([app.inert, bar.inert, tasks.inert], [true, true, true]);
  const panel = overlay.createLayer({kind: 'popover', id: 'tasks', el: node('panel', tasks)}); panel.open();
  assert.equal(tasks.inert, false, 'a layer opened on top stays usable');
  panel.close('api');
  assert.equal(tasks.inert, true);
  layer.close('api');
  assert.deepEqual([app.inert, bar.inert, tasks.inert], [false, false, false]);
  // 层被藏起来（没有走 close）时，下一次按键清栈，inert 一并去掉
  layer.open(); viewer.hidden = true; doc.key('Escape');
  assert.equal(app.inert, false);
});
test('onDismiss 返回 false 时层留在栈里（「放弃改动？」），之后照常关', () => {
  const d = dialogNode('profile'); let ask = true;
  overlay.openModal(d, {id: 'profile', onDismiss: () => { if (ask) return false; d.close(); }});
  doc.key('Escape'); assert.equal(d.open, true); assert.deepEqual(overlay.layerIds(), ['profile']);
  ask = false; doc.key('Escape'); assert.equal(d.open, false); assert.deepEqual(overlay.layerIds(), []);
});
