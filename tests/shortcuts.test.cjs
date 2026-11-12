// 快捷键名称跟随系统：Apple 系统上是 ⌘，其他系统是 Ctrl（core/core.js、ui/dom.js；#199）。
const {test, before} = require('node:test');
const assert = require('node:assert/strict');
const web = require('./web.cjs');
let core, dom;
before(async () => { await web.ready(); core = web.core(); dom = await web.load('ui/dom.js'); web.fakePage(); dom.wireDom(); });

test('Apple systems are told apart by their platform name', () => {
  for (const platform of ['MacIntel', 'iPhone', 'iPad']) assert.equal(core.macLike(platform), true, platform);
  for (const platform of ['Win32', 'Linux x86_64', '', undefined]) assert.equal(core.macLike(platform), false, String(platform));
});
for (const mac of [true, false]) {
  test(`shortcut help and titles ${mac ? 'on a Mac' : 'elsewhere'}`, () => {
    assert.equal(core.shortcut('Z', mac), mac ? '⌘Z' : 'Ctrl+Z');
    const nodes = [{nodeValue: '⌘K'}, {nodeValue: '⌘+ / ⌘− · 捏合'}];
    const button = {title: '跳到角色（⌘K）'};
    let index = 0;
    const walker = {nextNode() { this.currentNode = nodes[index++]; return !!this.currentNode; }};
    globalThis.NodeFilter = {SHOW_TEXT: 4};
    globalThis.document = {createTreeWalker: () => walker};
    dom.platformHints({querySelectorAll: () => [button]}, mac);
    assert.deepEqual(nodes.map(n => n.nodeValue), mac ? ['⌘K', '⌘+ / ⌘− · 捏合'] : ['Ctrl+K', 'Ctrl++ / Ctrl+− · 捏合']);
    assert.equal(button.title, mac ? '跳到角色（⌘K）' : '跳到角色（Ctrl+K）');
  });
}
test('the undo button names the step and the shortcut of this system', () => {
  const button = {};
  globalThis.document = {getElementById: id => id === 'undoButton' ? button : null};
  core.setLatest({id: 1, label: '合成撤销'});
  assert.equal(button.title, `撤销：合成撤销（${core.shortcut('Z')}）`);
  assert.equal(button.disabled, false);
  core.edits.busy = true; core.setLatest(core.edits.latest);
  assert.equal(button.disabled, true);
  core.edits.busy = false; core.setLatest(null);
  assert.equal(button.title, '暂无可撤销操作');
  assert.equal(button.disabled, true);
});
