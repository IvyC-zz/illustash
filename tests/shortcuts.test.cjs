const {test} = require('node:test');
const assert = require('node:assert/strict');
const {readFileSync} = require('node:fs');
const vm = require('node:vm');
const source = readFileSync(require('node:path').join(__dirname, '../web/static/app.js'), 'utf8');
const hints = source.slice(source.indexOf('const isMac ='), source.indexOf('const number ='));
for (const platform of ['MacIntel', 'Win32', 'Linux x86_64']) {
  test(`shortcut help and titles on ${platform}`, () => {
    const nodes = [{nodeValue:'⌘K'}, {nodeValue:'⌘+ / ⌘− · 捏合'}];
    const button = {title:'跳到角色（⌘K）'};
    let index = 0;
    const walker = {nextNode() { this.currentNode = nodes[index++]; return !!this.currentNode; }};
    const context = {navigator:{platform}, NodeFilter:{SHOW_TEXT:4}, document:{body:{querySelectorAll:() => [button]}, createTreeWalker:() => walker}};
    vm.createContext(context); vm.runInContext(hints, context);
    assert.equal(vm.runInContext("shortcut('Z')", context), platform === 'MacIntel' ? '⌘Z' : 'Ctrl+Z');
    const undoButton = {};
    context.state = {}; context.$ = () => undoButton;
    vm.runInContext(source.slice(source.indexOf('function setUndo('), source.indexOf('// ---------- routing')), context);
    vm.runInContext("setUndo({label:'合成撤销'})", context);
    assert.equal(undoButton.title, platform === 'MacIntel' ? '撤销：合成撤销（⌘Z）' : '撤销：合成撤销（Ctrl+Z）');
    assert.equal(nodes[0].nodeValue, platform === 'MacIntel' ? '⌘K' : 'Ctrl+K');
    assert.equal(button.title, platform === 'MacIntel' ? '跳到角色（⌘K）' : '跳到角色（Ctrl+K）');
  });
}
