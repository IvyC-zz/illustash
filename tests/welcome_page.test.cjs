// #95：桌面欢迎页。选中的文件夹当场检查；已被同来源的另一个文件夹读到的文件夹去掉，跨来源的重叠会指出来；
// 导入或新建失败时保留已填写的内容。
const {test} = require('node:test');
const assert = require('node:assert/strict');
const {readFileSync} = require('node:fs');
const vm = require('node:vm');
const read = name => readFileSync(require('node:path').join(__dirname, '../web/static', name), 'utf8');
// 与启动页相同的 t() 和 tb()：图库页的 i18n.js，用外壳的语言（#173）。
function i18n(language, tables = {}) {
  const context = {self: {ILLUSTASH_LANGUAGE: language, ILLUSTASH_LANGUAGES: Object.fromEntries(Object.keys(tables).map(c => [c, c])), ILLUSTASH_TABLES: tables},
    navigator: {languages: ['zh-CN']}, document: {documentElement: {}, querySelectorAll: () => []}};
  vm.createContext(context);
  vm.runInContext(read('i18n.js'), context);
  return {t: vm.runInContext('t', context), tb: vm.runInContext('tb', context)};
}
const zh = i18n('zh-CN');
const html = readFileSync(require('node:path').join(__dirname, '../desktop/dist/index.html'), 'utf8');
const script = [...html.matchAll(/<script>([\s\S]*?)<\/script>/g)].pop()[1];
const section = script.slice(script.indexOf('// 欢迎页：'), script.indexOf('// 外壳在服务启动失败时调用'));

function page(answers, words = zh) {
  const nodes = {}, calls = [];
  const c = {
    calls, nodes, setInterval: () => 0, clearInterval() {},
    document: {getElementById: id => nodes[id] ||= {innerHTML: '', value: ''}, body: {classList: {add() {}, remove() {}}}},
    busy() {}, window: {},
    invoke: async (cmd, args) => { calls.push([cmd, args]); const a = answers[cmd]; return typeof a === 'function' ? a(args) : a; },
  };
  vm.createContext(Object.assign(c, words));
  vm.runInContext(section, c);
  c.window.showWelcome({origins: [{id: 'x', label: 'X'}, {id: 'pixiv', label: 'Pixiv'}], recent: [], home: '/Users/me/Pictures'});
  return c;
}
const ok = (path, images, more = {}) => ({path, status: 'ok', images, partial: false, unreadable: 0, inside: null, counted: true, ...more});
const click = (c, data) => c.nodes.welcome.onclick({target: {closest: () => ({dataset: data})}});

test('a folder inside another of the same origin is not added twice', async () => {
  let picked = ['/a'];
  const c = page({
    welcome_pick: () => ({paths: picked}),
    welcome_inspect: ({paths}) => paths.map(p => p === '/a/b' ? ok(p, 0, {inside: '/a', counted: false}) : ok(p, 5)),
  });
  await click(c, {pick: 'x'});
  picked = ['/a/b'];
  await click(c, {pick: 'x'});
  assert.deepEqual(JSON.parse(vm.runInContext('JSON.stringify(W.seq)', c)), [['x', '/a']]);
  assert.match(c.nodes.wcol.innerHTML, /已包含在.*\/a.*里，不重复添加/);
  assert.match(c.nodes.wcol.innerHTML, /5 张图片/);
});

test('an overlap across origins stays and is pointed out; empty and unreadable folders are explained', async () => {
  let picked = ['/a'];
  const c = page({
    welcome_pick: () => ({paths: picked}),
    welcome_inspect: ({paths}) => paths.map(p => ({'/a': ok(p, 3, {partial: true, unreadable: 2}), '/a/b': ok(p, 1, {inside: '/a'}),
      '/empty': ok(p, 0, {status: 'empty'}), '/locked': ok(p, 0, {status: 'denied'})}[p])),
  });
  await click(c, {pick: 'x'});
  picked = ['/a/b', '/empty', '/locked'];
  await click(c, {pick: 'pixiv'});
  const shown = c.nodes.wcol.innerHTML;
  assert.equal(vm.runInContext('W.seq.length', c), 4);
  assert.match(shown, /和「X」里的 <bdi>\/a<\/bdi> 重叠/);
  assert.match(shown, /至少 3 张图片，还没数完，2 处读不到/);
  assert.match(shown, /没有找到图片/);
  assert.match(shown, /没有访问权限/);
});

test('a failed import and a failed creation keep the form as it was filled in', async () => {
  const c = page({
    welcome_pick: ({kind}) => ({paths: kind === 'zip' ? ['/Downloads/old.pilib.zip'] : ['/a']}),
    welcome_inspect: ({paths}) => paths.map(p => ok(p, 2)),
    welcome_import: () => { throw '导出文件里有不认识的内容，已拒绝导入'; },
    welcome_create: () => { throw '保存失败'; },
  });
  await click(c, {pick: 'x'});
  await click(c, {actW: 'import'});
  assert.equal(vm.runInContext('W.importing', c), null);
  assert.match(c.nodes.wcol.innerHTML, /已拒绝导入/);
  assert.deepEqual(JSON.parse(vm.runInContext('JSON.stringify(W.seq)', c)), [['x', '/a']]);
  await click(c, {actW: 'create'});
  assert.match(c.nodes.wcol.innerHTML, /保存失败/);
  assert.match(c.nodes.wcol.innerHTML, /data-drop="\/a"/);
  assert.deepEqual(JSON.parse(JSON.stringify(c.calls.find(([cmd]) => cmd === 'welcome_create')[1].sources)), [{kind: 'folder', path: '/a', preset: 'x'}]);
});

test('a library made but not listed is offered instead of making a second one (#97)', async () => {
  const c = page({
    welcome_pick: () => ({paths: ['/a']}),
    welcome_inspect: ({paths}) => paths.map(p => ok(p, 2)),
    welcome_create: () => ({created: '/Users/me/Pictures/Illustash/新图库.pilib', error: '写入失败（合成）'}),
    welcome_open: () => null,
  });
  await click(c, {pick: 'x'});
  await click(c, {actW: 'create'});
  const shown = c.nodes.wcol.innerHTML;
  assert.match(shown, /图库已经建好/);
  assert.match(shown, /写入失败（合成）/);
  assert.doesNotMatch(shown, /data-act-w="create"/, 'creating again is not the first offer');
  await click(c, {actW: 'open-created'});
  assert.deepEqual(JSON.parse(JSON.stringify(c.calls.at(-1))), ['welcome_open', {path: '/Users/me/Pictures/Illustash/新图库.pilib'}]);
  await click(c, {actW: 'create-again'});
  assert.match(c.nodes.wcol.innerHTML, /data-act-w="create"/);
});

test('a double click opens one picker and each import or creation carries its own request id (#115)', async () => {
  let choose;
  const c = page({
    welcome_pick: ({kind}) => kind === 'zip' ? new Promise(r => { choose = r; }) : {paths: ['/a']},
    welcome_inspect: ({paths}) => paths.map(p => ok(p, 2)),
    welcome_import: () => { throw '合成失败'; },
    welcome_create: () => { throw '合成失败'; },
  });
  const first = click(c, {actW: 'import'}), second = click(c, {actW: 'import'});
  await second;
  choose({paths: ['/Downloads/a.pilib.zip']});
  await first;
  assert.equal(c.calls.filter(([cmd]) => cmd === 'welcome_pick').length, 1);
  assert.equal(c.calls.filter(([cmd]) => cmd === 'welcome_import').length, 1);
  const again = click(c, {actW: 'import'});
  choose({paths: ['/Downloads/a.pilib.zip']});
  await again;
  await click(c, {pick: 'x'});
  await click(c, {actW: 'create'});
  const ids = c.calls.filter(([cmd]) => cmd === 'welcome_import' || cmd === 'welcome_create').map(([, a]) => a.request);
  assert.equal(ids.length, 3);
  assert.ok(ids.every(id => typeof id === 'string' && id.length > 4));
  assert.equal(new Set(ids).size, 3, 'a new click is a new request');
});

// #422：名字框里按 Enter 等于点「创建并打开」；组字中的 Enter 不算，创建进行中再按一次不会另建一个，
// 「图库已经建好」那一步（不画「创建并打开」）里按 Enter 什么也不做。
test('Enter in the name box creates the library once', async () => {
  let finish;
  const c = page({
    welcome_pick: () => ({paths: ['/a']}),
    welcome_inspect: ({paths}) => paths.map(p => ok(p, 2)),
    welcome_create: () => new Promise(r => { finish = r; }),
  });
  await click(c, {pick: 'x'});
  const enter = (more = {}) => c.nodes.welcome.onkeydown({key: 'Enter', target: {id: 'wname'}, preventDefault() {}, ...more});
  await enter({isComposing: true});
  await enter({keyCode: 229});
  await enter({target: {id: 'other'}});
  assert.equal(c.calls.filter(([cmd]) => cmd === 'welcome_create').length, 0);
  const first = enter(); await enter();
  assert.equal(c.calls.filter(([cmd]) => cmd === 'welcome_create').length, 1, 'a second Enter while creating is ignored');
  finish({created: '/Users/me/Pictures/Illustash/新图库.pilib', error: '写入失败（合成）'});
  await first;
  await enter();
  assert.equal(c.calls.filter(([cmd]) => cmd === 'welcome_create').length, 1, 'no second library from the "already made" step');
});

// #173 第 4 阶段：外壳在页面脚本之前注入它的语言和对应的表（desktop/src/i18n.rs），启动页加载图库页的
// i18n.js，用它们翻译自己的标记和欢迎页。
test('the start page speaks the language the shell gives it', async () => {
  const scripts = [...html.matchAll(/<script src="([^"]+)">/g)].map(m => m[1]);
  assert.deepEqual(scripts, ['i18n.js', 'splash.js']);
  const en = i18n('en', {en: JSON.parse(read('locales/en.json'))});
  const c = page({
    welcome_pick: () => ({paths: ['/a', '/b']}),
    welcome_inspect: ({paths}) => paths.map(p => ok(p, p === '/a' ? 1 : 1234)),
    welcome_create: () => { throw 'synthetic failure'; },
  }, en);
  c.window.showWelcome({origins: [{id: 'x', label: 'X'}, {id: 'other', label: '其他'}], recent: [], home: '/Users/me/Pictures'});
  assert.match(c.nodes.wcol.innerHTML, /No libraries yet/);
  assert.match(c.nodes.wcol.innerHTML, /Open a library…/);
  await click(c, {pick: 'x'});
  const shown = c.nodes.wcol.innerHTML;
  assert.match(shown, />1 picture</);
  assert.match(shown, />1,234 pictures</);
  assert.match(shown, /placeholder="New library"/);
  assert.doesNotMatch(shown, /\p{Script=Han}/u);
  // 名字留空时取占位文字，用页面的语言。
  await click(c, {actW: 'create'});
  assert.equal(c.calls.find(([cmd]) => cmd === 'welcome_create')[1].name, 'New library');
  // core 给的中文（来源名、错误）经过 tb()，与图库页相同。
  assert.match(c.nodes.wcol.innerHTML, /<h3>Other<\/h3>/);
  assert.match(c.nodes.wcol.innerHTML, /synthetic failure/);
  const zhPage = page({welcome_pick: () => ({paths: ['/a']}), welcome_inspect: ({paths}) => paths.map(p => ok(p, 1)), welcome_create: () => ({})});
  await click(zhPage, {pick: 'x'});
  await click(zhPage, {actW: 'create'});
  assert.equal(zhPage.calls.find(([cmd]) => cmd === 'welcome_create')[1].name, '新图库');
});
